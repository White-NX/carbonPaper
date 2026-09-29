import { useCallback, useLayoutEffect, useRef, useState } from 'react';

const BOTTOM_THRESHOLD = 32;

/** Follow a growing answer until the reader scrolls up or opens its history. */
export function useFollowOutput(resetKey) {
  const scrollRef = useRef(null);
  const contentRef = useRef(null);
  const followingRef = useRef(true);
  const lastPosition = useRef({ top: 0, height: 0, viewport: 0 });
  const [following, setFollowing] = useState(true);

  const scrollToBottom = useCallback(() => {
    const node = scrollRef.current;
    if (!node) return;
    node.scrollTop = Math.max(0, node.scrollHeight - node.clientHeight);
    lastPosition.current = { top: node.scrollTop, height: node.scrollHeight, viewport: node.clientHeight };
  }, []);

  const resume = useCallback(() => {
    followingRef.current = true;
    setFollowing(true);
    scrollToBottom();
  }, [scrollToBottom]);

  const pause = useCallback(() => {
    followingRef.current = false;
    setFollowing(false);
  }, []);

  // Wheel intent arrives before the browser's scroll event. Stop immediately so
  // a text delta in between cannot pull the reader back down.
  const onWheel = useCallback((event) => {
    if (event.deltaY < 0) pause();
  }, [pause]);

  const onScroll = useCallback((event) => {
    if (event.target !== event.currentTarget) return;
    const node = event.currentTarget;
    const previous = lastPosition.current;
    const resized = node.scrollHeight !== previous.height || node.clientHeight !== previous.viewport;
    // Layout changes (including auto-collapsing activity) can also emit scroll events.
    if (resized && followingRef.current) {
      scrollToBottom();
      return;
    }
    const movedUp = node.scrollTop < previous.top - 1;
    const atBottom = node.scrollHeight - node.clientHeight - node.scrollTop <= BOTTOM_THRESHOLD;
    const next = atBottom && !movedUp;
    followingRef.current = next;
    setFollowing(next);
    lastPosition.current = { top: node.scrollTop, height: node.scrollHeight, viewport: node.clientHeight };
  }, [scrollToBottom]);

  useLayoutEffect(() => { resume(); }, [resetKey, resume]);

  // Run before paint for streamed text; ResizeObserver also catches thumbnails,
  // disclosures and container resizes that do not render the panel itself.
  useLayoutEffect(() => {
    if (followingRef.current) scrollToBottom();
  });

  useLayoutEffect(() => {
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      if (followingRef.current) scrollToBottom();
    });
    if (scrollRef.current) observer.observe(scrollRef.current);
    if (contentRef.current) observer.observe(contentRef.current);
    return () => observer.disconnect();
  }, [scrollToBottom]);

  return { scrollRef, contentRef, following, onScroll, onWheel, resume, pause };
}
