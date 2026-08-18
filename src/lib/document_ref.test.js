import { describe, expect, it } from 'vitest';
import {
  getOfficeDocumentApplicationKey,
  getOfficeDocumentIcon,
  getOfficeDocumentKindKey,
} from './document_ref';

describe('document_ref presentation helpers', () => {
  it('maps Office applications to stable labels and icons', () => {
    expect(getOfficeDocumentApplicationKey('word')).toBe('word');
    expect(getOfficeDocumentApplicationKey('excel')).toBe('excel');
    expect(getOfficeDocumentApplicationKey('power_point')).toBe('powerPoint');
    expect(getOfficeDocumentIcon('word')).toBeTruthy();
    expect(getOfficeDocumentIcon('excel')).toBeTruthy();
    expect(getOfficeDocumentIcon('power_point')).toBeTruthy();
    expect(getOfficeDocumentIcon('unknown')).toBeTruthy();
  });

  it('maps saved and unsaved document kinds', () => {
    expect(getOfficeDocumentKindKey('local_file')).toBe('local');
    expect(getOfficeDocumentKindKey('cloud_document')).toBe('cloud');
    expect(getOfficeDocumentKindKey('unsaved')).toBe('unsaved');
    expect(getOfficeDocumentKindKey('unknown')).toBe('office');
  });
});
