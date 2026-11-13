use super::*;

struct TestService {
    manager: ServiceHandle,
    name: String,
}

impl TestService {
    fn new() -> Self {
        // SAFETY: this opens SCM for subsequent operations on a unique test name.
        let manager = unsafe {
            OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
                .expect("open SCM for the test service")
        };
        Self {
            manager: ServiceHandle(manager),
            name: format!("CarbonPaper.Test.Repair.{}", &random_id()[..16]),
        }
    }

    fn open(&self, access: u32) -> windows::core::Result<ServiceHandle> {
        let name = wide(self.name.as_ref());
        // SAFETY: the manager handle is live and the unique name is terminated.
        unsafe { OpenServiceW(self.manager.0, PCWSTR(name.as_ptr()), access).map(ServiceHandle) }
    }

    fn remove(&self) -> windows::core::Result<()> {
        // DELETE access is needed only for this fixture.
        let service = self.open(0x0001_0000)?;
        // SAFETY: the handle refers only to this fixture's unique service name.
        unsafe { DeleteService(service.0) }
    }
}

impl Drop for TestService {
    fn drop(&mut self) {
        // Also clean up when an assertion fails after service creation.
        let _ = self.remove();
    }
}

#[test]
#[ignore = "requires administrator access; creates and deletes a unique, never-started test service"]
fn existing_service_repair_reapplies_restart_policy() {
    // SAFETY: IsUserAnAdmin only queries the current process token.
    assert!(
        unsafe { IsUserAnAdmin() }.as_bool(),
        "run this test elevated"
    );
    let directory = tempfile::tempdir().expect("create test directory");
    // The service cannot execute code even if somebody attempts to start it.
    let missing_executable = directory.path().join("nonexistent-test-service.exe");
    let fixture = TestService::new();
    configure_named_service(&missing_executable, &fixture.name, &fixture.name)
        .expect("create test service through the installer");

    {
        let limited = fixture
            .open(SERVICE_CHANGE_CONFIG)
            .expect("open service with the old repair permissions");
        assert_eq!(
            configure_failure_actions(&limited),
            Err(BrokerError::AccessDenied),
            "restart actions require SERVICE_START in addition to configuration access"
        );
        // Clear recovery actions to ensure repair actually restores the policy.
        let mut unused_action = SC_ACTION::default();
        let empty = SERVICE_FAILURE_ACTIONSW {
            // A null pointer would leave the previous action array unchanged.
            lpsaActions: &mut unused_action,
            ..Default::default()
        };
        // SAFETY: SCM copies this initialized structure synchronously. With no
        // restart actions, SERVICE_CHANGE_CONFIG is sufficient for this call.
        unsafe {
            ChangeServiceConfig2W(
                limited.0,
                SERVICE_CONFIG_FAILURE_ACTIONS,
                Some((&empty as *const SERVICE_FAILURE_ACTIONSW).cast()),
            )
            .expect("clear recovery actions");
        }
    }

    configure_named_service(&missing_executable, &fixture.name, &fixture.name)
        .expect("repair the existing service through the installer");

    let context = crate::windows::update::CoordinatorContext::new().unwrap();
    context
        .with_permissions(|| {
            assert!(fixture.open(SERVICE_START | SERVICE_QUERY_STATUS).is_ok());
            for access in [SERVICE_STOP, SERVICE_CHANGE_CONFIG, 0x0004_0000] {
                assert!(
                    fixture.open(access).is_err(),
                    "a restricted client must not receive service mutation rights"
                );
            }
        })
        .unwrap();

    // Retain an ordinary client's START handle before maintenance changes SCM
    // configuration. Revoking its ACL afterward would not revoke that handle.
    {
        let cached_client = context
            .with_permissions(|| fixture.open(SERVICE_START | SERVICE_QUERY_STATUS).unwrap())
            .unwrap();
        let handle = fixture.open(MAINTENANCE_ACCESS).unwrap();
        set_service_start_type(&handle, SERVICE_DEMAND_START).unwrap();
        {
            let mut maintenance = ServiceMaintenance::from_handle(handle).unwrap();
            maintenance.pause().unwrap();
            configure_named_service_with_start_type(
                &missing_executable,
                &fixture.name,
                &fixture.name,
                SERVICE_DISABLED,
            )
            .unwrap();
            context
                .with_permissions(|| {
                    // SAFETY: this is the fixture's retained ordinary START handle;
                    // the executable does not exist and can never execute code.
                    let error = unsafe { StartServiceW(cached_client.0, None) }.unwrap_err();
                    assert_eq!(error.code(), windows::core::HRESULT::from_win32(1058));
                })
                .unwrap();
            assert_eq!(maintenance.start(), Err(BrokerError::Unavailable));
        }
        let restored = fixture.open(SERVICE_QUERY_CONFIG).unwrap();
        assert_eq!(service_start_type(&restored).unwrap(), SERVICE_DEMAND_START);
    }
    configure_named_service(&missing_executable, &fixture.name, &fixture.name).unwrap();

    {
        let service = fixture
            .open(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)
            .expect("query repaired service");
        let mut buffer = [0usize; 512];
        let mut needed = 0;
        // SAFETY: aligned storage is large enough for this fixed policy and is
        // retained while reading the returned structure and its action array.
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(
                buffer.as_mut_ptr().cast::<u8>(),
                std::mem::size_of_val(&buffer),
            );
            QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS,
                Some(bytes),
                &mut needed,
            )
            .expect("query failure actions");
            let policy = &*buffer.as_ptr().cast::<SERVICE_FAILURE_ACTIONSW>();
            assert_eq!(policy.dwResetPeriod, 24 * 60 * 60);
            assert_eq!(policy.cActions, 3);
            assert!(!policy.lpsaActions.is_null());
            let actions = std::slice::from_raw_parts(policy.lpsaActions, policy.cActions as usize);
            for (action, delay) in actions.iter().zip([5_000, 30_000, 60_000]) {
                assert_eq!(action.Type, SC_ACTION_RESTART);
                assert_eq!(action.Delay, delay);
            }
        }
        assert_eq!(
            service::query_status(&service)
                .expect("query service state")
                .dwCurrentState,
            SERVICE_STOPPED,
            "configuration must not start the test service"
        );
    }
    fixture.remove().expect("delete the test service");
}
