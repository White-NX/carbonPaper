export const FEATURE_MODE_OPTIONS = [
  {
    value: 'minimal',
    config: {
      classification_enabled: false,
      smart_cluster_enabled: false,
    },
  },
  {
    value: 'basic',
    config: {
      classification_enabled: true,
      smart_cluster_enabled: false,
    },
  },
  {
    value: 'smart',
    config: {
      classification_enabled: true,
      smart_cluster_enabled: true,
    },
  },
];

export function getFeatureMode(config) {
  const match = FEATURE_MODE_OPTIONS.find((option) => (
    Boolean(config.classification_enabled) === option.config.classification_enabled
    && Boolean(config.smart_cluster_enabled) === option.config.smart_cluster_enabled
  ));
  return match?.value || 'custom';
}
