import React from 'react';
import Timeline from './Timeline';
import { setPreference, usePreference } from '../lib/preference_store';

/** Keep each page's camera mounted so search navigation never replaces the home viewport. */
export default function WorkspaceTimelines({ activeTab, searchState, onSelectSearchResult, ...timelineProps }) {
  const recordsCollapsed = usePreference('recordsTimelineCollapsed', 'false') === 'true';
  const searchCollapsed = usePreference('searchTimelineCollapsed', 'false') === 'true';

  return (
    <>
      <Timeline {...timelineProps} active={activeTab === 'preview'}
        compact={recordsCollapsed}
        onToggleCompact={() => setPreference('recordsTimelineCollapsed', !recordsCollapsed)} />
      <Timeline
        {...timelineProps}
        active={activeTab === 'advanced-search'}
        jumpTimestamp={null}
        searchState={searchState}
        onSelectSearchResult={onSelectSearchResult}
        compact={searchCollapsed}
        onToggleCompact={() => setPreference('searchTimelineCollapsed', !searchCollapsed)}
      />
    </>
  );
}
