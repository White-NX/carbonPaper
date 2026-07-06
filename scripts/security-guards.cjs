const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const COMMAND_TIERS = {
  'commands::utility::greet': 'public',
  'commands::utility::close_process': 'public',
  'commands::utility::set_updating_flag': 'public',
  'commands::utility::set_app_language': 'public',
  'monitor::start_monitor': 'public',
  'monitor::stop_monitor': 'session_required',
  'monitor::pause_monitor': 'session_required',
  'monitor::resume_monitor': 'session_required',
  'monitor::get_monitor_status': 'public',
  'monitor::monitor_search_nl': 'session_required',
  'monitor::monitor_update_filters': 'session_required',
  'monitor::monitor_update_advanced_config': 'session_required',
  'monitor::monitor_update_feature_config': 'session_required',
  'monitor::monitor_get_all_models': 'session_required',
  'monitor::monitor_run_clustering': 'session_required',
  'monitor::monitor_get_clustering_status': 'session_required',
  'monitor::monitor_set_clustering_interval': 'session_required',
  'monitor::monitor_get_task_clusters': 'session_required',
  'monitor::monitor_nl_cluster_query': 'session_required',
  'monitor::monitor_nl_cluster_reranker_status': 'session_required',
  'monitor::monitor_smart_cluster_worker_status': 'session_required',
  'monitor::monitor_smart_cluster_drain_now': 'session_required',
  'monitor::monitor_smart_cluster_stop_drain': 'session_required',
  'monitor::monitor_smart_cluster_calibrate_preview': 'session_required',
  'monitor::monitor_presidio_set_language': 'session_required',
  'monitor::monitor_classify_debug': 'session_required',
  'monitor::monitor_remove_local_anchors_by_process': 'session_required',
  'script_integrity::debug_trigger_security_alert': 'public',
  'commands::storage::storage_get_timeline': 'session_required',
  'commands::storage::storage_get_timeline_density': 'session_required',
  'commands::storage::storage_search': 'session_required',
  'commands::storage::storage_get_image': 'session_required',
  'commands::storage::storage_get_thumbnail': 'session_required',
  'commands::storage::storage_batch_get_thumbnails': 'session_required',
  'commands::storage::storage_warmup_thumbnails': 'session_required',
  'commands::storage::storage_get_thumbnail_warmup_status': 'session_required',
  'commands::storage::storage_cancel_thumbnail_warmup': 'session_required',
  'commands::storage::storage_get_screenshot_details': 'session_required',
  'commands::storage::storage_delete_screenshot': 'session_required',
  'commands::storage::storage_delete_by_time_range': 'session_required',
  'commands::storage::storage_list_processes': 'session_required',
  'commands::storage::storage_get_process_stats': 'session_required',
  'commands::storage::storage_get_process_monthly_thumbnails': 'session_required',
  'commands::storage::storage_soft_delete': 'session_required',
  'commands::storage::storage_soft_delete_screenshots': 'session_required',
  'commands::storage::storage_get_delete_queue_status': 'session_required',
  'commands::storage::storage_get_index_health': 'session_required',
  'commands::storage::storage_retry_vector_indexing': 'session_required',
  'commands::storage::storage_save_screenshot': 'session_required',
  'commands::storage::storage_set_policy': 'session_required',
  'commands::storage::storage_get_policy': 'session_required',
  'commands::storage::storage_get_public_key': 'public',
  'commands::storage::storage_compute_link_scores': 'public',
  'commands::storage::storage_encrypt_for_chromadb': 'public',
  'commands::storage::storage_decrypt_from_chromadb': 'session_required',
  'commands::storage::storage_update_category': 'session_required',
  'commands::storage::storage_get_categories': 'session_required',
  'commands::storage::storage_get_categories_from_db': 'session_required',
  'commands::storage::storage_batch_get_categories': 'session_required',
  'commands::migration::storage_get_startup_vacuum_status': 'public',
  'commands::migration::storage_run_startup_vacuum_if_needed': 'public',
  'commands::migration::storage_run_manual_vacuum': 'session_required',
  'commands::migration::storage_check_hmac_migration_status': 'public',
  'commands::migration::storage_run_hmac_migration': 'session_required',
  'commands::migration::storage_hmac_migration_cancel': 'session_required',
  'commands::migration::storage_export_backup': 'session_required',
  'commands::migration::storage_import_backup': 'session_required',
  'commands::storage::storage_get_tasks': 'session_required',
  'commands::storage::storage_get_related_screenshots': 'session_required',
  'commands::storage::storage_get_task_screenshots': 'session_required',
  'commands::storage::storage_update_task_label': 'session_required',
  'commands::storage::storage_delete_task': 'session_required',
  'commands::storage::storage_remove_task_screenshot': 'session_required',
  'commands::storage::storage_merge_tasks': 'session_required',
  'commands::storage::storage_save_clustering_results': 'session_required',
  'analysis::get_analysis_overview': 'session_required',
  'commands::mcp::mcp_set_enabled': 'session_required',
  'commands::mcp::mcp_get_status': 'public',
  'commands::mcp::mcp_ack_privacy_warning': 'public',
  'commands::mcp::mcp_reset_token': 'session_required',
  'commands::mcp::mcp_copy_token_to_clipboard': 'session_required',
  'commands::mcp::mcp_get_port': 'public',
  'commands::mcp::mcp_set_port': 'session_required',
  'commands::mcp::mcp_get_sensitive_filter_config': 'session_required',
  'commands::mcp::mcp_set_sensitive_filter_config': 'session_required',
  'commands::utility::get_advanced_config': 'public',
  'commands::utility::set_advanced_config': 'session_required',
  'monitor::enumerate_gpus': 'public',
  'commands::utility::toggle_game_mode': 'session_required',
  'commands::utility::get_game_mode_status': 'public',
  'commands::migration::storage_list_plaintext_files': 'session_required',
  'commands::migration::storage_migrate_plaintext': 'session_required',
  'commands::migration::storage_migrate_data_dir': 'session_required',
  'commands::migration::storage_migration_cancel': 'session_required',
  'commands::migration::storage_delete_plaintext': 'session_required',
  'commands::credential::credential_initialize': 'public',
  'commands::credential::credential_verify_user': 'public',
  'commands::credential::credential_check_session': 'public',
  'commands::credential::credential_lock_session': 'public',
  'commands::credential::credential_set_foreground': 'public',
  'commands::credential::credential_set_session_timeout': 'session_required',
  'commands::credential::credential_get_session_timeout': 'public',
  'get_autostart_status': 'public',
  'set_autostart': 'public',
  'python::check_python_status': 'public',
  'python::check_python_venv': 'public',
  'python::request_install_python': 'public',
  'python::install_python_venv': 'public',
  'python::check_deps_freshness': 'public',
  'python::sync_python_deps': 'public',
  'python::install_spacy_model': 'public',
  'python::check_spacy_models': 'public',
  'python::force_recheck_spacy_models': 'public',
  'model_management::download_model': 'public',
  'model_management::check_model_files': 'public',
  'updater::updater_check': 'public',
  'updater::updater_download': 'public',
  'updater::updater_extract': 'public',
  'updater::updater_apply': 'public',
  'native_messaging::get_nm_host_status': 'public',
  'native_messaging::register_nm_host_chrome': 'public',
  'native_messaging::register_nm_host_edge': 'public',
  'native_messaging::install_browser_extension': 'public',
  'native_messaging::sync_extension_if_needed': 'public',
  'commands::utility::check_extension_setup_needed': 'public',
  'commands::utility::mark_extension_setup_done': 'public',
  'commands::utility::check_clustering_setup_needed': 'public',
  'commands::utility::mark_clustering_setup_done': 'public',
  'commands::utility::check_smart_cluster_setup_needed': 'public',
  'commands::utility::mark_smart_cluster_setup_done': 'public',
  'commands::utility::get_extension_enhancement_config': 'public',
  'commands::utility::set_extension_enhancement': 'session_required',
  'commands::smart_cluster::smart_cluster_list': 'session_required',
  'commands::smart_cluster::smart_cluster_get': 'session_required',
  'commands::smart_cluster::smart_cluster_get_examples': 'session_required',
  'commands::smart_cluster::smart_cluster_create': 'session_required',
  'commands::smart_cluster::smart_cluster_delete': 'session_required',
  'commands::smart_cluster::smart_cluster_update_anchor': 'session_required',
  'commands::smart_cluster::smart_cluster_update_threshold': 'session_required',
  'commands::smart_cluster::smart_cluster_toggle_enabled': 'session_required',
  'commands::smart_cluster::smart_cluster_assignments': 'session_required',
  'commands::smart_cluster::smart_cluster_ocr_corpus': 'session_required',
  'commands::smart_cluster::smart_cluster_get_summary': 'session_required',
  'commands::smart_cluster::smart_cluster_upsert_summary': 'session_required',
  'commands::smart_cluster::smart_cluster_delete_summary': 'session_required',
  'commands::smart_cluster::smart_cluster_rescan': 'session_required',
  'commands::smart_cluster::smart_cluster_rescan_all': 'session_required',
  'commands::smart_cluster::smart_cluster_clear_assignments': 'session_required',
  'commands::smart_cluster::smart_cluster_status': 'session_required',
  'idle::get_idle_state': 'public',
  'commands::utility::get_log_dir': 'public',
  'commands::utility::restart_app': 'public',
  'commands::utility::trigger_test_error': 'public',
  'commands::utility::exit_app': 'public',
  'commands::utility::frontend_log': 'public',
  'commands::utility::switch_to_lightweight_mode': 'public',
  'commands::utility::switch_to_standard_mode': 'public',
  'commands::utility::get_lightweight_status': 'public',
  'commands::utility::get_lightweight_config': 'public',
  'commands::utility::set_lightweight_config': 'public',
  'commands::utility::open_path': 'public',
  'power::get_power_saving_status': 'public',
  'power::set_power_saving_enabled': 'public',
};

function read(file) {
  return fs.readFileSync(path.join(ROOT, file), 'utf8');
}

function registeredCommands() {
  const lib = read('src-tauri/src/lib.rs');
  const match = lib.match(/generate_handler!\s*\[([\s\S]*?)\]\);/);
  if (!match) throw new Error('Could not find tauri::generate_handler! list');
  return match[1]
    .split('\n')
    .map((line) => line.replace(/\/\/[^\r\n]*/, '').trim().replace(/,$/, ''))
    .filter(Boolean);
}

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === 'dist') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

function checkCommandPolicies() {
  const commands = registeredCommands();
  const missing = commands.filter((cmd) => !COMMAND_TIERS[cmd]);
  const stale = Object.keys(COMMAND_TIERS).filter((cmd) => !commands.includes(cmd));
  const invalid = Object.entries(COMMAND_TIERS)
    .filter(([, tier]) => !['public', 'session_required'].includes(tier))
    .map(([cmd]) => cmd);

  if (missing.length || stale.length || invalid.length) {
    throw new Error([
      missing.length ? `Missing command policy:\n${missing.join('\n')}` : '',
      stale.length ? `Stale command policy:\n${stale.join('\n')}` : '',
      invalid.length ? `Invalid command policy tier:\n${invalid.join('\n')}` : '',
    ].filter(Boolean).join('\n\n'));
  }
}

function checkDomSinks() {
  const sinkPattern = /\b(dangerouslySetInnerHTML|innerHTML|insertAdjacentHTML|eval\s*\(|new\s+Function\s*\()/;
  const offenders = walk(path.join(ROOT, 'src'))
    .filter((file) => /\.(js|jsx|ts|tsx)$/.test(file))
    .flatMap((file) => {
      const rel = path.relative(ROOT, file);
      return fs.readFileSync(file, 'utf8').split(/\r?\n/).flatMap((line, idx) => {
        if (!sinkPattern.test(line)) return [];
        return [`${rel}:${idx + 1}: ${line.trim()}`];
      });
    });
  if (offenders.length) {
    throw new Error(`High-risk DOM sink found:\n${offenders.join('\n')}`);
  }
}

checkCommandPolicies();
checkDomSinks();
console.log('Security guards passed');
