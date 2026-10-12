import js from '@eslint/js';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';

const frontendFiles = ['src/**/*.{js,jsx}', 'browser-extension/**/*.{js,jsx}'];
const frontendTests = ['src/**/*.test.{js,jsx}', 'src/tests/**'];
const translationRestrictions = [{
  selector: 'CallExpression:matches([callee.name="t"], [callee.property.name="t"])[arguments.1.type="Literal"][arguments.1.value=type(string)]',
  message: 'Keep translation defaults in src/i18n/locales instead of inline strings.',
}];

export default [
  {
    ignores: [
      'dist/**',
      'coverage/**',
      'node_modules/**',
      'src-tauri/**',
      'monitor/**',
    ],
  },
  {
    files: frontendFiles,
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
      globals: {
        ...globals.browser,
        ...globals.es2021,
      },
    },
    settings: {
      react: { version: 'detect' },
    },
    plugins: {
      react,
      'react-hooks': reactHooks,
    },
    rules: {
      ...js.configs.recommended.rules,
      ...react.configs.flat['jsx-runtime'].rules,
      // Keep the established Hooks checks. The newer React Compiler rules are
      // intentionally opt-in because this codebase is not compiler-adopted.
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
      'react/jsx-key': 'error',
      'react/jsx-no-duplicate-props': 'error',
      'react/jsx-no-undef': 'error',
      'react/no-unknown-property': 'error',
      'no-restricted-syntax': ['error', ...translationRestrictions],

      // Existing components intentionally accept flexible data-shaped props.
      'react/prop-types': 'off',
      'react/no-unescaped-entities': 'off',
      'no-unused-vars': 'off',
      'no-console': 'off',

      // These rules have existing intentional/legacy cases. Keep them out of
      // the initial gate so the rollout can focus on correctness regressions.
      'no-empty': 'off',
      'no-prototype-builtins': 'off',
      'no-useless-escape': 'off',
      'no-extra-boolean-cast': 'off',
      'no-useless-catch': 'off',
    },
  },
  {
    files: ['src/**/*.{js,jsx}'],
    ignores: [...frontendTests, 'src/lib/native_dialogs.js'],
    rules: {
      'no-restricted-syntax': ['error', ...translationRestrictions, {
        selector: ':matches(ImportDeclaration, ExportNamedDeclaration, ExportAllDeclaration, ImportExpression)[source.value="@tauri-apps/plugin-dialog"]',
        message: 'Use the named file-selection helpers in src/lib/native_dialogs.js.',
      }, {
        selector: 'CallExpression[callee.name="require"][arguments.0.value="@tauri-apps/plugin-dialog"]',
        message: 'Use the named file-selection helpers in src/lib/native_dialogs.js.',
      }],
    },
  },
  {
    files: ['src/**/*.{js,jsx}'],
    ignores: [...frontendTests, 'src/components/Dialog.jsx', 'src/components/ConfirmDialog.jsx',
      'src/components/overlay/OverlayShell.jsx', 'src/components/ErrorWindow.jsx'],
    rules: {
      'no-restricted-imports': ['error', { patterns: [{
        group: ['**/useDialogFocus', '**/useDialogFocus.js'],
        importNames: ['useDialogFocus'],
        message: 'Use Dialog, ConfirmDialog or OverlayShell to inherit visibility, layers and focus behavior.',
      }] }],
    },
  },
  {
    files: ['src/**/*.test.{js,jsx}'],
    languageOptions: {
      globals: {
        ...globals.node,
        ...globals.vitest,
      },
    },
  },
  {
    files: ['browser-extension/**/*.test.{js,jsx}'],
    languageOptions: {
      globals: {
        ...globals.node,
        ...globals.vitest,
      },
    },
  },
  {
    files: ['browser-extension/**/*.js'],
    languageOptions: {
      globals: {
        chrome: 'readonly',
      },
    },
  },
  {
    files: ['browser-extension/background.js'],
    languageOptions: {
      globals: {
        ...globals.serviceworker,
        chrome: 'readonly',
      },
    },
  },
];
