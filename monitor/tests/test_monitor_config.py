import importlib

import monitor.config as config


def test_feature_toggles_initialize_from_environment(monkeypatch):
    monkeypatch.setenv("CARBONPAPER_CLUSTERING_ENABLED", "false")
    monkeypatch.setenv("CARBONPAPER_CLASSIFICATION_ENABLED", "0")

    reloaded = importlib.reload(config)

    assert reloaded.CLUSTERING_ENABLED is False
    assert reloaded.CLASSIFICATION_ENABLED is False

    monkeypatch.delenv("CARBONPAPER_CLUSTERING_ENABLED", raising=False)
    monkeypatch.delenv("CARBONPAPER_CLASSIFICATION_ENABLED", raising=False)
    importlib.reload(config)


def test_feature_toggles_default_enabled_without_environment(monkeypatch):
    monkeypatch.delenv("CARBONPAPER_CLUSTERING_ENABLED", raising=False)
    monkeypatch.delenv("CARBONPAPER_CLASSIFICATION_ENABLED", raising=False)

    reloaded = importlib.reload(config)

    assert reloaded.CLUSTERING_ENABLED is True
    assert reloaded.CLASSIFICATION_ENABLED is True
