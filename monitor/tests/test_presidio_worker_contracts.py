from monitor.presidio_worker import PresidioWorker


def test_presidio_worker_analyze_payload_contract(monkeypatch):
    worker = PresidioWorker()
    calls = []

    def fake_request(payload, timeout=14.0):
        calls.append({"payload": payload, "timeout": timeout})
        return {"status": "success", "results": [{"entities": []}]}

    monkeypatch.setattr(worker, "request", fake_request)

    assert worker.analyze(
        ["contact me"],
        language="en",
        entity_types=["EMAIL_ADDRESS"],
        timeout=3.0,
    ) == [{"entities": []}]
    assert calls == [
        {
            "payload": {
                "command": "analyze",
                "texts": ["contact me"],
                "language": "en",
                "entity_types": ["EMAIL_ADDRESS"],
            },
            "timeout": 3.0,
        }
    ]


def test_presidio_worker_lifecycle_payload_contract(monkeypatch):
    worker = PresidioWorker()
    calls = []
    responses = {
        "status": {"status": "success", "initialized": True, "language": "zh-CN", "model": "zh_core_web_sm"},
        "unload": {"status": "success", "unloaded": True},
        "check_idle": {"status": "success", "unloaded": False},
    }

    worker._proc = type("Proc", (), {"is_alive": lambda self: True})()
    worker._conn = object()

    def fake_request(payload, timeout=14.0):
        calls.append({"payload": payload, "timeout": timeout})
        return responses[payload["command"]]

    monkeypatch.setattr(worker, "request", fake_request)

    assert worker.status()["initialized"] is True
    assert worker.unload()["unloaded"] is True
    assert worker.check_idle()["unloaded"] is False

    assert calls == [
        {"payload": {"command": "status"}, "timeout": 2.0},
        {"payload": {"command": "unload"}, "timeout": 5.0},
        {"payload": {"command": "check_idle"}, "timeout": 5.0},
    ]
