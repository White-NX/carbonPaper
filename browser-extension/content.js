// CarbonPaper Browser Extension - Content Script
// Collects visible links, page URL, title, and favicon from the current page

(function() {
  'use strict';

  /**
   * Check if an element is at least partially visible in the viewport
   */
  function isElementVisible(elem) {
    const rect = elem.getBoundingClientRect();
    return rect.bottom > 0 && rect.top < window.innerHeight &&
           rect.right > 0 && rect.left < window.innerWidth &&
           rect.width > 0 && rect.height > 0;
  }

  /**
   * Get all visible links in the current viewport.
   * Extracts from <a> tags as well as URLs found in text content of
   * <p>, <span>, <li>, <td>, <div>, <h1>-<h6>, and <label> elements.
   */
  function getVisibleLinks() {
    // Use a Map so duplicate URLs can have their text upgraded in-place
    const linkMap = new Map();

    // 1. Extract from <a href> tags (primary source)
    for (const link of document.querySelectorAll('a[href]')) {
      try {
        const href = link.href;
        if (!href ||
            href.startsWith('javascript:') ||
            href.startsWith('data:') ||
            href.startsWith('vbscript:') ||
            href.startsWith('#')) continue;
        if (!isElementVisible(link)) continue;

        // Try title from <a> itself, then from child elements, then textContent
        let text = link.title;
        if (!text) {
          const titledChild = link.querySelector('[title]');
          if (titledChild) text = titledChild.title;
        }
        text = (text || link.textContent || '').trim().substring(0, 200) || href;

        const existing = linkMap.get(href);
        if (!existing) {
          linkMap.set(href, { text, url: href });
        } else if (text !== href && (existing.text === href || existing.text === existing.url)) {
          // Current <a> has a real title while the earlier one only had the URL — upgrade
          existing.text = text;
        }
      } catch (e) {
        // Skip problematic elements silently
      }
    }

    const result = Array.from(linkMap.values());
    const seen = new Set(linkMap.keys());

    // 2. Extract URLs from text content in common container elements
    const urlPattern = /https?:\/\/[^\s<>"')\]},;]+/g;
    const textContainers = document.querySelectorAll(
      'p, span, li, td, div, h1, h2, h3, h4, h5, h6, label'
    );

    for (const elem of textContainers) {
      if (!isElementVisible(elem)) continue;

      // Collect only direct text nodes (excluding text inside child <a> tags)
      // This avoids duplicating URLs already captured from <a href> elements
      let directText = '';
      for (const node of elem.childNodes) {
        if (node.nodeType === Node.TEXT_NODE) {
          directText += node.textContent;
        } else if (node.nodeType === Node.ELEMENT_NODE && node.tagName !== 'A') {
          // Include text from non-anchor child elements, but skip any
          // nested <a> tags within them
          const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT, {
            acceptNode(n) {
              let parent = n.parentElement;
              while (parent && parent !== elem) {
                if (parent.tagName === 'A') return NodeFilter.FILTER_REJECT;
                parent = parent.parentElement;
              }
              return NodeFilter.FILTER_ACCEPT;
            }
          });
          while (walker.nextNode()) {
            directText += walker.currentNode.textContent;
          }
        }
      }

      if (!directText || directText.length > 5000) continue;

      let match;
      urlPattern.lastIndex = 0;
      while ((match = urlPattern.exec(directText)) !== null) {
        // Clean trailing punctuation that likely isn't part of the URL
        let url = match[0].replace(/[.,;:!?)]+$/, '');
        if (seen.has(url)) continue;

        seen.add(url);
        // Extract surrounding context as link text
        const startIdx = Math.max(0, match.index - 40);
        const endIdx = Math.min(directText.length, match.index + url.length + 40);
        const context = directText.substring(startIdx, endIdx).trim().substring(0, 200);
        result.push({ text: context, url });
      }
    }

    return result;
  }

  /**
   * Get the page's favicon URL
   */
  function getFavicon() {
    const iconLink = document.querySelector('link[rel~="icon"]') ||
                     document.querySelector('link[rel="shortcut icon"]');
    if (iconLink && iconLink.href) {
      return iconLink.href;
    }
    // Default to /favicon.ico
    return new URL('/favicon.ico', window.location.origin).href;
  }

  // Listen for messages from the background script
  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type === 'getPageData') {
      sendResponse({
        url: window.location.href,
        title: document.title,
        favicon: getFavicon(),
        visibleLinks: getVisibleLinks()
      });
    }
    return true; // Keep the message channel open for async response
  });
})();
