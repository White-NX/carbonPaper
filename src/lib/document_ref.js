import { FileSpreadsheet, FileText, Presentation } from 'lucide-react';

/**
 * The backend exposes the Office application as a stable enum.  Keep the
 * presentation mapping here instead of guessing from display_name, which is
 * absent for some unsaved documents and can be localized by Office.
 */
export function getOfficeDocumentIcon(application) {
  switch (application) {
    case 'excel':
      return FileSpreadsheet;
    case 'power_point':
    case 'powerpoint':
      return Presentation;
    case 'word':
    default:
      return FileText;
  }
}

export function getOfficeDocumentApplicationKey(application) {
  switch (application) {
    case 'excel':
      return 'excel';
    case 'power_point':
    case 'powerpoint':
      return 'powerPoint';
    case 'word':
      return 'word';
    default:
      return 'office';
  }
}

export function getOfficeDocumentKindKey(kind) {
  switch (kind) {
    case 'local_file':
      return 'local';
    case 'cloud_document':
      return 'cloud';
    case 'unsaved':
      return 'unsaved';
    default:
      return 'office';
  }
}
