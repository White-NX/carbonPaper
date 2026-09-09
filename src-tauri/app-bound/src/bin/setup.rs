#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    #[cfg(windows)]
    {
        let result = run();
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> carbonpaper_app_bound::Result<()> {
    use carbonpaper_app_bound::{
        protocol::BrokerError,
        windows::install::{self, InstallOptions},
    };
    let launch_args = std::env::args().skip(1).collect::<Vec<_>>();
    if launch_args.first().is_some_and(|s| s == "--launch-after") {
        if launch_args.len() != 2 {
            return Err(BrokerError::InvalidRequest);
        }
        return install::launch_after(
            launch_args[1]
                .parse()
                .map_err(|_| BrokerError::InvalidRequest)?,
        );
    }
    let mut source = None;
    let mut pid = None;
    let mut enable = false;
    let mut uninstall = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--source" if source.is_none() => {
                source = Some(std::path::PathBuf::from(
                    args.next().ok_or(BrokerError::InvalidRequest)?,
                ))
            }
            "--caller-pid" if pid.is_none() => {
                pid = Some(
                    args.next()
                        .ok_or(BrokerError::InvalidRequest)?
                        .parse::<u32>()
                        .map_err(|_| BrokerError::InvalidRequest)?,
                )
            }
            "--enable" if !enable => enable = true,
            "--uninstall" if !uninstall => uninstall = true,
            _ => return Err(BrokerError::InvalidRequest),
        }
    }
    let caller_pid = pid.ok_or(BrokerError::InvalidRequest)?;
    if uninstall {
        if source.is_some() || enable {
            return Err(BrokerError::InvalidRequest);
        }
        install::uninstall(caller_pid)
    } else {
        install::install(InstallOptions {
            source: source.ok_or(BrokerError::InvalidRequest)?,
            caller_pid,
            enable,
        })
        .map(|_| ())
    }
}
