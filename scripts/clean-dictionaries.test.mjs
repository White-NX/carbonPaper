import assert from 'node:assert/strict';
import test from 'node:test';
import { decide, safeError } from './clean-dictionaries.mjs';

const entry = { id: 'fixture', length: 8, sources: ['cat_02', 'cat_04'] };
const keep = { action: 'keep', category: 'cat_04' };

test('provider failure preserves a baseline trigger instead of silently allowing it', () => {
  assert.deepEqual(decide(entry, { action: 'remove' }), {
    action: 'keep', category: 'cat_04', route: 'provider_unresolved_legacy',
  });
});

test('independent semantic agreement can resolve a provider failure', () => {
  const neutral = { action: 'remove', category: 'none', confidence: 0.99, benign: 0.99 };
  assert.equal(decide(entry, neutral, undefined, undefined, neutral).action, 'remove');
  assert.equal(decide(entry, neutral, undefined, undefined, { ...neutral, action: 'review' }).route, 'provider_unresolved_legacy');
});

test('a keep requires agreement on disposition and domain', () => {
  assert.equal(decide(entry, keep, keep, keep).action, 'keep');
  assert.equal(decide(entry, keep, { ...keep, category: 'cat_02' }, keep).action, 'context');
  assert.equal(decide(entry, keep, { action: 'remove', category: 'none' }, keep).action, 'context');
  assert.equal(decide(entry, keep, keep, { action: 'context', category: 'cat_04' }).action, 'context');
});

test('review uncertainty stays explicit and inactive short terms stay inactive', () => {
  assert.equal(decide(entry, keep, keep, { action: 'review', category: 'none' }).action, 'review');
  assert.equal(decide({ ...entry, length: 1 }).action, 'remove');
});

test('errors cannot echo provider bodies, paths, or secrets', () => {
  for (const message of ['provider echoed: private fixture', 'C:/private/fixture', 'sk-fixture-secret']) {
    assert.equal(safeError(new Error(message)), 'LOCAL_OPERATION_FAILED');
  }
  assert.equal(safeError(new Error('DEEPSEEK_CONTENT_REJECTION_400')), 'DEEPSEEK_CONTENT_REJECTION_400');
});
