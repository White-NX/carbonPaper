import React from 'react';
import Timeline from './Timeline';
import { setPreference, usePreference } from '../lib/preference_store';

/** Keep each page's camera mounted so search navigation never replaces the home viewport. */
export default function WorkspaceTimelines({ activeTab, searchState, onSelectSearchResult, ...timelineProps }) {
  const searchCollapsed = usePreference('searchTimelineCollapsed', 'false') === 'true';

  return (
    <>
      <Timeline {...timelineProps} active={activeTab === 'preview'} />
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
