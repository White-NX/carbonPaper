"""Only the remaining Python clustering consumer receives feature configuration."""
import monitor as mm


def test_feature_config_updates_clustering_without_a_classification_worker(monkeypatch):
    monkeypatch.setattr(mm, "_auth_token", None)
    monkeypatch.setattr(mm.config, "CLUSTERING_ENABLED", True)
    result = mm._handle_command_impl({"command": "update_feature_config", "clustering_enabled": False})
    assert result == {"status": "success", "clustering_enabled": False}
    assert mm.config.CLUSTERING_ENABLED is False
    assert not hasattr(mm.config, "CLASSIFICATION_ENABLED")
