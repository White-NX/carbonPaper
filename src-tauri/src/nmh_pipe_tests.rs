//! Native regressions use random pipe names, owned child processes and synthetic
//! requests. No application registry, auth-token file or screenshot store is used.

use super::*;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncReadExt;

const TOKEN: &str = "synthetic-nmh-test-token";
const CHILD_TEST: &str = "nmh_pipe::tests::native_pipe_child";

fn pipe_name() -> String {
    format!(
        r"\\.\pipe\CarbonPaper.NmhTest.{}",
        hex::encode(rand::random::<[u8; 16]>())
    )
}

fn child(role: &str, name: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env("CARBONPAPER_NMH_TEST_ROLE", role)
        .env("CARBONPAPER_NMH_TEST_PIPE", name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap()
}

fn finish(mut child: Child) -> String {
    let deadline = Instant::now() + Duration::from_secs(12);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("isolated NMH child timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

// This ignored fixture is invoked only as an owned subprocess by native tests.
#[test]
#[ignore]
fn native_pipe_child() {
    let Ok(role) = std::env::var("CARBONPAPER_NMH_TEST_ROLE") else {
        return;
    };
    let name = std::env::var("CARBONPAPER_NMH_TEST_PIPE").unwrap();
    match role.as_str() {
        "additional" => {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: create an additional instance of only the test-owned name.
            let raw = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    1024 * 1024,
                    1024 * 1024,
                    0,
                    None,
                )
            };
            assert!(
                raw.is_invalid(),
                "same-user process created an additional server"
            );
            let error = io::Error::last_os_error().raw_os_error().unwrap();
            assert!(matches!(error, 5 | 231), "unexpected create error: {error}");
            println!("additional_instance_denied={error}");
        }
        "requests" | "wrong_pid" => {
            let client = NmhClient::for_test(std::env::current_exe().unwrap());
            for step in 0..2 {
                let response = client
                    .send(
                        &name,
                        &serde_json::json!({
                            "command": "echo", "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                            "auth_token": TOKEN,
                            "nmh_pid": std::process::id() + u32::from(role == "wrong_pid"),
                            "image_data": "synthetic-image", "step": step,
                        }),
                    )
                    .unwrap();
                if role == "wrong_pid" {
                    assert_eq!(response["error"], "Invalid NMH client identity");
                } else {
                    assert_eq!(response["status"], "success");
                    assert_eq!(response["data"]["nmh_pid"], std::process::id());
                    assert_eq!(response["data"]["step"], step);
                }
            }
            println!(
                "channel={}",
                client.channel.lock().unwrap().as_ref().unwrap().name
            );
            println!("requests_ok");
        }
        "receive" => {
            use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;
            use windows::Win32::System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe};
            let mut file = create_server_file(&name, false).unwrap();
            let handle = HANDLE(file.as_raw_handle());
            println!("receiver_ready");
            io::stdout().flush().unwrap();
            for _ in 0..2 {
                // SAFETY: the fixture retains its synchronous server handle.
                if let Err(error) = unsafe { ConnectNamedPipe(handle, None) } {
                    assert_eq!(error.code(), ERROR_PIPE_CONNECTED.to_hresult());
                }
                let mut byte = [0u8];
                let received =
                    with_io_timeout(handle, Duration::from_secs(5), || file.read(&mut byte))
                        .unwrap();
                assert_eq!(received, 0, "rejected receiver got sensitive request bytes");
                // SAFETY: no I/O remains pending and the same handle is reused.
                unsafe {
                    DisconnectNamedPipe(handle).unwrap();
                }
            }
            println!("sensitive_bytes_received=0");
        }
        "legacy" => {
            let mut file = open_client(&name).unwrap();
            println!("legacy_request_started");
            io::stdout().flush().unwrap();
            let _ = exchange(
                &mut file,
                &serde_json::json!({
                    "command": "register_nmh", "ipc_protocol_version": 2,
                    "nmh_pid": std::process::id(), "auth_token": TOKEN,
                }),
            );
            panic!("authenticated legacy host should have been stopped for reconnection");
        }
        _ => panic!("unknown native fixture role"),
    }
}

async fn broker(
    name: &str,
    handler: RequestHandler,
) -> (mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let server = async_server(create_server_file(name, true).unwrap()).unwrap();
    let verifier = PeerVerifier::for_test(std::env::current_exe().unwrap());
    let (stop, receive) = mpsc::channel(1);
    let task = tokio::spawn(serve_broker(
        server,
        verifier,
        receive,
        TOKEN.into(),
        handler,
        Arc::new(|_| {}),
    ));
    (stop, task)
}

async fn stop(stop: mpsc::Sender<()>, task: tokio::task::JoinHandle<()>) {
    stop.send(()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_server_denies_additional_instances_and_owner_dacl_writes() {
    let name = pipe_name();
    let server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let output = tokio::task::spawn_blocking({
        let name = name.clone();
        move || finish(child("additional", &name))
    })
    .await
    .unwrap();
    assert!(output.contains("additional_instance_denied="));
    // Same-user ownership must not implicitly grant WRITE_DAC to a pipe client.
    let error = OpenOptions::new()
        .access_mode(0x0004_0000)
        .open(&name)
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(5));
    drop(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_verification_checks_kernel_user_and_creation_time() {
    let name = pipe_name();
    let server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let file = open_client(&name).unwrap();
    server.connect().await.unwrap();
    let verifier = PeerVerifier::for_test(std::env::current_exe().unwrap());
    let handle = HANDLE(file.as_raw_handle());
    let peer = verifier.verify(handle, PipeSide::Client, None).unwrap();
    assert_eq!(peer.pid, std::process::id());
    let mut reused = peer.clone();
    reused.created_at += 1;
    assert!(verifier
        .verify(handle, PipeSide::Client, Some(&reused))
        .is_err());
    let mut wrong_user = verifier;
    wrong_user.user_sid = "S-1-5-0".into();
    assert!(wrong_user
        .verify(handle, PipeSide::Client, Some(&peer))
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn independent_nmh_channels_overlap_and_reuse_their_server_handles() {
    let name = pipe_name();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let received = Arc::new(Mutex::new(HashMap::<u32, usize>::new()));
    let handler: RequestHandler = Arc::new({
        let received = received.clone();
        move |peer, request| {
            let barrier = barrier.clone();
            let received = received.clone();
            Box::pin(async move {
                if request["step"] == 0 {
                    tokio::time::timeout(Duration::from_secs(5), barrier.wait())
                        .await
                        .unwrap();
                }
                *received.lock().unwrap().entry(peer.pid).or_default() += 1;
                StorageResponse::success(serde_json::json!({
                    "nmh_pid": peer.pid, "step": request["step"],
                }))
            })
        }
    });
    let (shutdown, task) = broker(&name, handler).await;
    let a = child("requests", &name);
    let b = child("requests", &name);
    let (a, b) = tokio::join!(
        tokio::task::spawn_blocking(move || finish(a)),
        tokio::task::spawn_blocking(move || finish(b)),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert!(a.contains("requests_ok") && b.contains("requests_ok"));
    let endpoint = |output: String| {
        output
            .lines()
            .find(|line| line.starts_with("channel="))
            .unwrap()
            .to_string()
    };
    assert_ne!(endpoint(a), endpoint(b));
    let counts = received.lock().unwrap().clone();
    assert_eq!(counts.len(), 2);
    assert!(counts.values().all(|count| *count == 2));
    stop(shutdown, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_channel_rejects_claimed_pid_before_dispatch() {
    let name = pipe_name();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler: RequestHandler = Arc::new({
        let calls = calls.clone();
        move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { StorageResponse::success(serde_json::json!({})) })
        }
    });
    let (shutdown, task) = broker(&name, handler).await;
    let output = tokio::task::spawn_blocking({
        let name = name.clone();
        move || finish(child("wrong_pid", &name))
    })
    .await
    .unwrap();
    assert!(output.contains("requests_ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(shutdown, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_bootstrap_image_receives_no_token_or_image_bytes() {
    let name = pipe_name();
    let mut server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let client = NmhClient::for_test(
        std::env::current_exe()
            .unwrap()
            .with_file_name("unexpected-peer.exe"),
    );
    let task = tokio::task::spawn_blocking({
        let name = name.clone();
        move || {
            client.send(
                &name,
                &serde_json::json!({
                    "auth_token": TOKEN, "image_data": "synthetic-image",
                }),
            )
        }
    });
    server.connect().await.unwrap();
    let mut byte = [0u8];
    let bytes = tokio::time::timeout(Duration::from_secs(5), server.read(&mut byte))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, 0);
    assert!(task
        .await
        .unwrap()
        .unwrap_err()
        .contains("Unexpected pipe peer executable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn private_receiver_with_same_image_and_user_but_wrong_pid_receives_no_bytes() {
    let name = pipe_name();
    let private = format!(
        "{}{}",
        nmh_protocol::DATA_PIPE_PREFIX,
        hex::encode(rand::random::<[u8; 16]>())
    );
    let mut receiver = child("receive", &private);
    let mut output = BufReader::new(receiver.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.contains("receiver_ready") {
            break;
        }
    }
    let mut server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let fake_broker = tokio::spawn(async move {
        for _ in 0..2 {
            server.connect().await.unwrap();
            let request: serde_json::Value =
                serde_json::from_slice(&read_server_ipc_frame(&mut server).await.unwrap()).unwrap();
            send_nmh_response(
                &mut server,
                StorageResponse::success(serde_json::json!({
                    "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                    "nmh_pid": request["nmh_pid"], "data_pipe_name": private,
                })),
            )
            .await;
            server.disconnect().unwrap();
        }
    });
    let client = NmhClient::for_test(std::env::current_exe().unwrap());
    let error = tokio::task::spawn_blocking(move || {
        client.send(
            &name,
            &serde_json::json!({
                "auth_token": TOKEN, "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                "nmh_pid": std::process::id(), "image_data": "synthetic-image",
            }),
        )
    })
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("Pipe peer process identity changed"));
    fake_broker.await.unwrap();
    // Read the remaining output separately because readiness consumed stdout.
    let remaining = tokio::task::spawn_blocking(move || {
        let mut remaining = String::new();
        output.read_to_string(&mut remaining).unwrap();
        let _ = finish(receiver);
        remaining
    })
    .await
    .unwrap();
    assert!(remaining.contains("sensitive_bytes_received=0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_channel_recovers_after_broker_restart_and_token_rotation() {
    let name = pipe_name();
    let client = Arc::new(NmhClient::for_test(std::env::current_exe().unwrap()));
    let echo: RequestHandler = Arc::new(|_, _| {
        Box::pin(async { StorageResponse::success(serde_json::json!({"saved": true})) })
    });
    let (shutdown, task) = broker(&name, echo.clone()).await;
    let send = |client: Arc<NmhClient>, name: String, token: String| {
        tokio::task::spawn_blocking(move || {
            client.send(
                &name,
                &serde_json::json!({
                    "command": "echo", "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                    "nmh_pid": std::process::id(), "auth_token": token,
                }),
            )
        })
    };
    assert_eq!(
        send(client.clone(), name.clone(), TOKEN.into())
            .await
            .unwrap()
            .unwrap()["status"],
        "success"
    );
    client.registered.store(true, Ordering::SeqCst);
    let old_channel = client
        .channel
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .name
        .clone();
    stop(shutdown, task).await;
    // The bootstrap handle can take one IOCP retirement tick to be fully closed.
    let deadline = Instant::now() + Duration::from_secs(3);
    let file = loop {
        match create_server_file(&name, true) {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(5) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(e) => panic!("{e}"),
        }
    };
    let server = async_server(file).unwrap();
    let (shutdown, receive) = mpsc::channel(1);
    let task = tokio::spawn(serve_broker(
        server,
        PeerVerifier::for_test(std::env::current_exe().unwrap()),
        receive,
        "rotated-synthetic-token".into(),
        echo,
        Arc::new(|_| {}),
    ));
    assert_eq!(
        send(client.clone(), name, "rotated-synthetic-token".into())
            .await
            .unwrap()
            .unwrap()["status"],
        "success"
    );
    assert_ne!(
        client.channel.lock().unwrap().as_ref().unwrap().name,
        old_channel
    );
    assert!(!client.registered.load(Ordering::SeqCst));
    stop(shutdown, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_legacy_host_is_stopped_for_native_port_reconnection() {
    let name = pipe_name();
    let (shutdown, task) = broker(
        &name,
        Arc::new(|_, _| panic!("legacy traffic must not reach screenshot/session dispatch")),
    )
    .await;
    let output = tokio::task::spawn_blocking({
        let name = name.clone();
        move || finish(child("legacy", &name))
    })
    .await
    .unwrap();
    assert!(output.contains("legacy_request_started"));
    stop(shutdown, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_pipe_read_is_cancelled_at_its_io_deadline() {
    let name = pipe_name();
    let server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let task = tokio::task::spawn_blocking(move || {
        let mut client = open_client(&name).unwrap();
        let started = Instant::now();
        let handle = HANDLE(client.as_raw_handle());
        let error = with_io_timeout(handle, Duration::from_millis(100), || {
            client.read_exact(&mut [0u8])
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(995));
        assert!(started.elapsed() < Duration::from_secs(2));
    });
    server.connect().await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_timeout_cancels_io_started_after_the_deadline() {
    use tokio::io::AsyncWriteExt;

    let name = pipe_name();
    let mut server = async_server(create_server_file(&name, true).unwrap()).unwrap();
    let mut task = tokio::task::spawn_blocking(move || {
        let mut client = open_client(&name).unwrap();
        let handle = HANDLE(client.as_raw_handle());
        let error = with_io_timeout(handle, Duration::from_millis(50), || {
            // No synchronous I/O is pending when the watchdog first expires.
            std::thread::sleep(Duration::from_millis(250));
            client.read_exact(&mut [0u8])
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(995));
    });
    server.connect().await.unwrap();
    let result = tokio::select! {
        result = &mut task => result,
        _ = tokio::time::sleep(Duration::from_secs(2)) => {
            // Release a missed cancellation so a regression fails without
            // leaving a blocked thread behind in the test process.
            let _ = server.write_all(&[0]).await;
            task.await
        }
    };
    result.unwrap();
}
