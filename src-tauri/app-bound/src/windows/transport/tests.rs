use super::*;
use std::{os::windows::io::FromRawHandle, sync::mpsc, thread};
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{Foundation::ERROR_PIPE_CONNECTED, Storage::FileSystem::PIPE_ACCESS_DUPLEX},
};

/// Exercise the real Windows byte-pipe behavior without contacting the broker.
fn pipe_pair() -> (File, File) {
    let name = format!(r"\\.\pipe\CarbonPaper.TransportTest.{}", random_id());
    let wide = identity::wide(name.as_ref());
    // SAFETY: the terminated name lives through creation, and the resulting
    // synchronous handle is transferred exactly once to the owning File.
    let server = unsafe {
        let handle = CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            MAX_FRAME_BYTES as u32,
            MAX_FRAME_BYTES as u32,
            1000,
            None,
        );
        assert!(!handle.is_invalid(), "{}", std::io::Error::last_os_error());
        File::from_raw_handle(handle.0)
    };
    let client = std::fs::OpenOptions::new()
        .access_mode(PIPE_CLIENT_ACCESS)
        .custom_flags((SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION).0)
        .open(name)
        .expect("open test pipe client");
    // SAFETY: both Files own live synchronous handles. Opening the client before
    // ConnectNamedPipe is documented to return ERROR_PIPE_CONNECTED.
    unsafe {
        if let Err(error) = ConnectNamedPipe(HANDLE(server.as_raw_handle()), None) {
            assert_eq!(error.code(), HRESULT::from_win32(ERROR_PIPE_CONNECTED.0));
        }
        SetNamedPipeHandleState(
            HANDLE(client.as_raw_handle()),
            Some(&PIPE_NOWAIT),
            None,
            None,
        )
        .expect("use the production client's nonblocking read mode");
    }
    (client, server)
}

#[test]
fn delayed_fragmented_reply_is_read_in_full() {
    let (mut client, mut server) = pipe_pair();
    let payload = b"delayed response";
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(payload);
    let (release, keep_open) = mpsc::channel::<()>();
    let writer = thread::spawn(move || -> std::io::Result<()> {
        thread::sleep(Duration::from_millis(50));
        // Split both the length prefix and the payload, leaving empty-pipe
        // intervals between writes as the production async server can do.
        for chunk in frame.chunks(2) {
            server.write_all(chunk)?;
            thread::sleep(Duration::from_millis(10));
        }
        // Windows may discard unread bytes when its server handle closes.
        let _ = keep_open.recv_timeout(Duration::from_secs(5));
        Ok(())
    });

    let reply = read_frame(&mut client, Instant::now() + Duration::from_secs(3));
    drop(release);
    writer.join().expect("writer thread").expect("write reply");
    assert_eq!(reply.expect("read delayed reply").as_slice(), payload);
}

#[test]
fn empty_connected_pipe_waits_until_deadline() {
    let (mut client, _server) = pipe_pair();
    let started = Instant::now();
    let timeout = Duration::from_millis(50);

    assert_eq!(
        read_frame(&mut client, started + timeout),
        Err(BrokerError::Unavailable)
    );
    assert!(
        started.elapsed() >= timeout,
        "an empty connected pipe must not be treated as EOF"
    );
}

#[test]
fn closed_pipe_fails_without_waiting_for_deadline() {
    let (mut client, server) = pipe_pair();
    drop(server);
    let started = Instant::now();

    assert_eq!(
        read_frame(&mut client, started + Duration::from_secs(3)),
        Err(BrokerError::Unavailable)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn truncated_reply_fails_when_the_peer_closes() {
    let (mut client, mut server) = pipe_pair();
    server
        .write_all(&8_u32.to_le_bytes())
        .expect("write length");
    server.write_all(b"part").expect("write partial payload");
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        drop(server);
    });
    let started = Instant::now();
    let result = read_frame(&mut client, started + Duration::from_secs(3));
    writer.join().expect("writer thread");

    assert_eq!(result, Err(BrokerError::Unavailable));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn invalid_frame_lengths_are_rejected() {
    for length in [0, MAX_FRAME_BYTES as u32 + 1] {
        let (mut client, mut server) = pipe_pair();
        server
            .write_all(&length.to_le_bytes())
            .expect("write length");

        assert_eq!(
            read_frame(&mut client, Instant::now() + Duration::from_secs(3)),
            Err(BrokerError::LimitExceeded)
        );
    }
}
