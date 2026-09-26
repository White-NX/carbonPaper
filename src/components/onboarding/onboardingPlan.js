import { FEATURE_MODE_OPTIONS } from '../settings/organize/featureModes';
import { MODEL_DOWNLOAD_MB } from '../../lib/modelSizes';

export const WIZARD_STEPS = ['welcome', 'features', 'browserStartup', 'review'];

export const FEATURE_MODES = FEATURE_MODE_OPTIONS.map((option) => option.value);

/** Same pairs as the resource policy card in general settings. */
export const RESOURCE_POLICIES = {
  complete: { powerSaving: false, gameMode: false },
  balanced: { powerSaving: true, gameMode: false },
  performance: { powerSaving: true, gameMode: true },
};

export const RERANKER_MODEL_ID = 'bge-reranker-v2-m3';

/**
 * The choices "use recommended settings" applies. Browsers follow detection:
 * installing an extension for a browser that is not there helps nobody.
 * Background processing after restart is recommended wherever it is offered.
 */
export function recommendedChoices({ browsers = {}, appBoundAvailable = false } = {}) {
  return {
    featureMode: 'smart',
    resourcePolicy: 'performance',
    browsers: { chrome: Boolean(browsers.chrome), edge: Boolean(browsers.edge) },
    launchAtLogin: true,
    autoStartCapture: true,
    appBound: Boolean(appBoundAvailable),
  };
}

export function featureConfig(featureMode) {
  return FEATURE_MODE_OPTIONS.find((option) => option.value === featureMode)?.config
    ?? FEATURE_MODE_OPTIONS[0].config;
}

export function needsReranker(choices, rerankerInstalled) {
  return featureConfig(choices.featureMode).smart_cluster_enabled && !rerankerInstalled;
}

export function selectedBrowsers(choices) {
  return Object.entries(choices.browsers).filter(([, on]) => on).map(([id]) => id);
}

/**
 * What finishing the wizard will do, in order. Quick settings first; the
 * browser extension next because it opens a browser window; the smart-cluster
 * download is queued to run in the background; the component that keeps
 * working after a restart goes last because it asks for administrator
 * approval and restarts the app.
 */
export function buildTasks(choices, { rerankerInstalled = false, appBoundAvailable = false } = {}) {
  const tasks = [
    { id: 'features' },
    { id: 'resources' },
    { id: 'startup' },
  ];
  if (selectedBrowsers(choices).length) tasks.push({ id: 'browsers' });
  if (needsReranker(choices, rerankerInstalled)) {
    tasks.push({ id: 'reranker', background: true, sizeMb: MODEL_DOWNLOAD_MB[RERANKER_MODEL_ID] });
  }
  if (appBoundAvailable && choices.appBound) tasks.push({ id: 'appBound', restarts: true });
  return tasks;
}

/**
 * Entries for the short "what's new" page existing users see once. Each entry
 * is shown only when it would change something for this user.
 */
export const WHATS_NEW_ENTRIES = [
  {
    id: 'smartCluster',
    applies: ({ smartClusterEnabled }) => !smartClusterEnabled,
    sizeMb: MODEL_DOWNLOAD_MB[RERANKER_MODEL_ID],
  },
  {
    id: 'appBound',
    applies: ({ appBoundAvailable }) => appBoundAvailable,
  },
];

export function applicableWhatsNew(context) {
  return WHATS_NEW_ENTRIES.filter((entry) => entry.applies(context));
}
