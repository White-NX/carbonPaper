export const AGENT_SKILL_NAME = 'carbonpaper-memory';
export const AGENT_SKILL_REPO = 'https://github.com/White-NX/carbonPaperSkill';
export const AGENT_SETUP_VARIANTS = ['codex', 'claude', 'cursor', 'generic'];
export const DEFAULT_AGENT_SETUP_VARIANT = 'codex';
export const CONTENT_FILTER_LEVELS = ['standard', 'minimal', 'off'];
export const CONTENT_FILTER_CATEGORIES = ['cat_01', 'cat_02', 'cat_03', 'cat_04', 'cat_05'];
export const CONTENT_FILTER_MODES = ['remove_paragraph', 'reject', 'mask'];
export const DEFAULT_CONTENT_FILTER_MODE = 'remove_paragraph';
// Mirrors PiiKind::SELECTABLE in src-tauri/src/pii/mod.rs.
export const PII_ENTITY_TYPES = [
  'PHONE_NUMBER',
  'CN_ID_CARD',
  'CN_BANK_CARD',
  'EMAIL_ADDRESS',
  'ADDRESS',
  'CREDENTIAL',
  'IP_ADDRESS',
];
export const DEFAULT_PII_ENTITIES = Object.fromEntries(
  PII_ENTITY_TYPES.map((type) => [type, type !== 'IP_ADDRESS']),
);
