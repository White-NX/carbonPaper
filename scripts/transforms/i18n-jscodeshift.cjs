const path = require('path');

// ── Attribute filter ──────────────────────────────────────────────
// Only these JSX attributes carry user-visible text worth translating.
const TRANSLATABLE_ATTRS = new Set([
  'title',
  'placeholder',
  'alt',
  'aria-label',
  'aria-description',
  'aria-placeholder',
  'aria-roledescription',
  'aria-valuetext',
  'label',
]);

// ── Text filter ───────────────────────────────────────────────────
// A string is considered translatable if it contains at least one
// CJK character OR one Latin letter (i.e. real words, not just
// CSS classes, numbers, or punctuation).
const HAS_WORD_CHAR = /[\p{Letter}]/u;

function isTranslatableText(str) {
  if (!str || !str.trim()) return false;
  return HAS_WORD_CHAR.test(str.trim());
}

// ── Component detection ───────────────────────────────────────────
// React components start with uppercase; skip plain helper functions.
function isComponentName(name) {
  return /^[A-Z]/.test(name);
}

// ── Main transform ───────────────────────────────────────────────
module.exports = function (fileInfo, api) {
  const j = api.jscodeshift;
  const root = j(fileInfo.source);
  const filePath = fileInfo.path;
  const rel = path.relative(process.cwd(), filePath).replace(/\\/g, '/');
  const fileKeyPrefix = rel
    .replace(/^src\//, '')
    .replace(/[^a-zA-Z0-9_\\/]/g, '_')
    .replace(/\\/g, '.');

  let counter = 0;
  let needsImport = false; // track whether we actually transformed anything

  // ── Ensure `import { useTranslation } from 'react-i18next'` ────
  const ensureImport = () => {
    if (!needsImport) return;
    const hasImport = root
      .find(j.ImportDeclaration, { source: { value: 'react-i18next' } })
      .some((p) =>
        p.node.specifiers.some(
          (s) => s.imported && s.imported.name === 'useTranslation',
        ),
      );
    if (!hasImport) {
      root
        .get()
        .node.program.body.unshift(
          j.importDeclaration(
            [j.importSpecifier(j.identifier('useTranslation'))],
            j.literal('react-i18next'),
          ),
        );
    }
  };

  // ── Inject `const { t } = useTranslation()` into a function body ─
  const addUseTranslationHook = (funcBody) => {
    if (!funcBody || !funcBody.body) return;
    const body = funcBody.body;
    const alreadyHas = body.some(
      (node) =>
        node.type === 'VariableDeclaration' &&
        node.declarations.some(
          (d) =>
            d.id &&
            d.id.type === 'ObjectPattern' &&
            d.id.properties.some((p) => p.key && p.key.name === 't'),
        ),
    );
    if (!alreadyHas) {
      const decl = j.variableDeclaration('const', [
        j.variableDeclarator(
          j.objectPattern([
            j.property('init', j.identifier('t'), j.identifier('t')),
          ]),
          j.callExpression(j.identifier('useTranslation'), []),
        ),
      ]);
      body.unshift(decl);
    }
  };

  // Collect component function nodes so we can inject the hook later
  // only in components that actually got text replaced.
  const componentNodes = new Set();

  // ── Find component functions ────────────────────────────────────
  // `function MyComponent(…) { … }`
  root.find(j.FunctionDeclaration).forEach(({ node }) => {
    if (node.id && isComponentName(node.id.name)) {
      componentNodes.add(node);
    }
  });

  // `export default function(…) { … }` (anonymous default export)
  root.find(j.ExportDefaultDeclaration).forEach(({ node }) => {
    const decl = node.declaration;
    if (
      decl &&
      (decl.type === 'FunctionDeclaration' ||
        decl.type === 'FunctionExpression' ||
        decl.type === 'ArrowFunctionExpression')
    ) {
      componentNodes.add(decl);
    }
  });

  // `const MyComponent = (…) => { … }` or `const MyComponent = function(…) { … }`
  root.find(j.VariableDeclaration).forEach((p) => {
    p.node.declarations.forEach((d) => {
      if (
        d.id &&
        d.id.name &&
        isComponentName(d.id.name) &&
        d.init &&
        (d.init.type === 'ArrowFunctionExpression' ||
          d.init.type === 'FunctionExpression')
      ) {
        componentNodes.add(d.init);
      }
    });
  });

  // ── Replace JSXAttribute string literals ────────────────────────
  root.find(j.JSXAttribute).forEach((p) => {
    const attr = p.node;
    const name = attr.name && attr.name.name;
    if (!name || !TRANSLATABLE_ATTRS.has(name)) return;
    if (!attr.value) return;

    let literalValue = null;
    if (attr.value.type === 'Literal') {
      literalValue = attr.value.value;
    } else if (
      attr.value.type === 'JSXExpressionContainer' &&
      attr.value.expression.type === 'Literal'
    ) {
      literalValue = attr.value.expression.value;
    }

    if (typeof literalValue === 'string' && isTranslatableText(literalValue)) {
      counter += 1;
      needsImport = true;
      const key = `${fileKeyPrefix}.${name}_${counter}`;
      const call = j.callExpression(j.identifier('t'), [
        j.literal(key),
        j.objectExpression([
          j.property(
            'init',
            j.identifier('defaultValue'),
            j.literal(literalValue),
          ),
        ]),
      ]);
      attr.value = j.jsxExpressionContainer(call);
    }
  });

  // ── Replace JSXText nodes ──────────────────────────────────────
  root.find(j.JSXText).forEach((p) => {
    const raw = p.node.value;
    if (!isTranslatableText(raw)) return;

    counter += 1;
    needsImport = true;
    const key = `${fileKeyPrefix}.text_${counter}`;
    const trimmed = raw.trim();
    const call = j.callExpression(j.identifier('t'), [
      j.literal(key),
      j.objectExpression([
        j.property(
          'init',
          j.identifier('defaultValue'),
          j.literal(trimmed),
        ),
      ]),
    ]);
    j(p).replaceWith(j.jsxExpressionContainer(call));
  });

  // ── Inject hook & import only if transforms happened ────────────
  if (needsImport) {
    ensureImport();
    componentNodes.forEach((node) => {
      try {
        addUseTranslationHook(node.body);
      } catch (_) {
        /* skip arrow functions with expression bodies */
      }
    });
  }

  return root.toSource({ quote: 'single' });
};
