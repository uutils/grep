use std::process::Stdio;
use std::time::{Duration, Instant};
use uutests::util::UChild;

use super::ucmd;

// Captured output is read by uutests in the background. Poll with a deadline
// while keeping stdin open, so EOF cannot accidentally satisfy the test.
fn wait_for_output(child: &mut UChild, stderr: bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = if stderr {
            child.stderr_bytes()
        } else {
            child.stdout_bytes()
        };
        if !output.is_empty() || Instant::now() >= deadline {
            return output;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn terminal_flushes_matching_lines_before_eof() {
    use std::io::Read;
    use std::sync::mpsc;
    use uutests::util::pty_path;

    for (options, expected) in [
        (vec![], "x\n"),
        (vec!["--color=never"], "x\n"),
        (vec!["--color=always"], "\x1b[01;31m\x1b[Kx\x1b[m\x1b[K\n"),
        (vec!["--line-buffered", "--color=never"], "x\n"),
    ] {
        let (_scene, mut command) = ucmd();
        let (_, mut controller, replica) = pty_path();
        // Read the PTY directly: TerminalSimulation buffers its captured output.
        let expected = expected.replace('\n', "\r\n").into_bytes();
        let length = expected.len();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = vec![0; length];
            let result = controller.read_exact(&mut bytes).map(|()| bytes);
            let _ = sender.send(result);
        });
        command
            .set_stdin(Stdio::piped())
            .set_stdout(replica)
            .args(&options)
            .arg("x")
            .env("GREP_COLORS", "")
            .env("GREP_COLOR", "")
            .timeout(Duration::from_secs(5));
        let mut child = command.run_no_wait();
        drop(command);
        let written = child.try_write_in("x\n");
        let early = receiver.recv_timeout(Duration::from_secs(5));
        // Kill on missing/short output so the PTY reader also reaches EOF.
        if !matches!(&early, Ok(Ok(_))) {
            child.kill();
        }
        // Reap and join before asserting, including the regression case.
        let result = child.wait().unwrap();
        reader.join().unwrap();
        written.unwrap();
        assert_eq!(early.unwrap().unwrap(), expected, "options: {options:?}");
        result.success().no_stderr();
    }
}

#[test]
fn explicit_line_buffering_flushes_pipe_before_eof() {
    let (_scene, mut command) = ucmd();
    let mut child = command
        .set_stdin(Stdio::piped())
        .args(&["--line-buffered", "--color=never", "x"])
        .timeout(Duration::from_secs(5))
        .run_no_wait();
    child.write_in("x\n");
    let early = wait_for_output(&mut child, false);
    child.wait().unwrap().success().no_stderr();
    assert_eq!(early, b"x\n");
}

#[test]
fn redirected_output_remains_buffered_by_default() {
    let (scene, mut command) = ucmd();
    scene.fixtures.write("first", "x\n");
    let mut child = command
        .set_stdin(Stdio::piped())
        .args(&["--color=never", "-h", "x", "first", "missing", "-"])
        .timeout(Duration::from_secs(5))
        .run_no_wait();
    // The missing-file diagnostic follows the first file's match. It proves
    // grep has processed that match before we check that stdout is buffered.
    let ready = wait_for_output(&mut child, true);
    std::thread::sleep(Duration::from_millis(200));
    let early = child.stdout_bytes();
    let result = child.wait().unwrap();
    result.code_is(2).stdout_is("x\n");
    assert!(!ready.is_empty(), "missing-file readiness diagnostic");
    assert!(early.is_empty(), "redirected output flushed before EOF");
}
