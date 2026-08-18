//! Isolated STA worker for Microsoft Office document discovery.

#[path = "../office_com.rs"]
mod office_com;
#[allow(dead_code)]
#[path = "../office_protocol.rs"]
mod office_protocol;
#[allow(dead_code)]
#[path = "../office_window.rs"]
mod office_window;

use office_protocol::{
    read_request, write_response, OfficeRequest, OfficeResponse, OFFICE_PROTOCOL_VERSION,
};
use std::io::{BufReader, BufWriter};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .init();

    let apartment = match office_com::OfficeComApartment::initialize() {
        Ok(apartment) => apartment,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    if std::env::args().any(|arg| arg == "--probe") {
        println!(
            "{}",
            serde_json::json!({
                "status": "ready",
                "protocol_version": OFFICE_PROTOCOL_VERSION,
                "worker_version": env!("CARGO_PKG_VERSION"),
                "apartment": "sta"
            })
        );
        drop(apartment);
        return;
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    if let Err(error) = write_response(
        &mut writer,
        &OfficeResponse::Ready {
            protocol_version: OFFICE_PROTOCOL_VERSION,
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    ) {
        eprintln!("failed to send Office worker handshake: {error}");
        return;
    }

    loop {
        let request = match read_request(&mut reader) {
            Ok(request) => request,
            Err(error) => {
                tracing::debug!("Office worker transport closed: {error}");
                break;
            }
        };
        let request_id = request.request_id();
        let should_shutdown = matches!(request, OfficeRequest::Shutdown { .. });
        let response = match request {
            OfficeRequest::Ping { request_id } => OfficeResponse::Pong { request_id },
            OfficeRequest::Shutdown { request_id } => OfficeResponse::ShuttingDown { request_id },
            OfficeRequest::Resolve {
                request_id,
                generation,
                application,
                root_hwnd,
                document_hwnd,
                pid,
                title,
            } => {
                let started = Instant::now();
                match catch_unwind(AssertUnwindSafe(|| {
                    office_com::resolve_document(application, root_hwnd, document_hwnd, pid, &title)
                })) {
                    Ok(Ok((resolved_hwnd, document))) => OfficeResponse::Resolved {
                        request_id,
                        generation,
                        document_hwnd: resolved_hwnd,
                        document,
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                    },
                    Ok(Err(error)) => OfficeResponse::Error {
                        request_id,
                        generation: Some(generation),
                        kind: error.kind,
                        message: error.message.chars().take(4096).collect(),
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                    },
                    Err(_) => OfficeResponse::Error {
                        request_id,
                        generation: Some(generation),
                        kind: "panic".to_string(),
                        message: "Office automation worker panicked while resolving a document"
                            .to_string(),
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                    },
                }
            }
        };

        if let Err(error) = write_response(&mut writer, &response) {
            eprintln!("failed to write Office response for request {request_id}: {error}");
            break;
        }
        if should_shutdown {
            break;
        }
    }
}
