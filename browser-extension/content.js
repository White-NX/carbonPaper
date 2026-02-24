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
   * Character-level Shannon entropy (bits/char).
   * Ported from link_scoring.rs — natural language typically falls in [3.0, 5.0].
   */
  function charEntropy(text) {
    const freq = {};
    let total = 0;
    for (const ch of text) {
      freq[ch] = (freq[ch] || 0) + 1;
      total++;
    }
    if (total === 0) return 0;
    let entropy = 0;
    for (const count of Object.values(freq)) {
      const p = count / total;
      entropy -= p * Math.log2(p);
    }
    return entropy;
  }

  /**
   * Gaussian penalty for entropy outside the natural-language sweet spot.
   * Adapted from link_scoring.rs (center=4.0, sigma=1.5).
   *
   * For short strings (len ≤ 8), Shannon entropy is inherently capped by
   * log2(len), so we normalize to the theoretical maximum before applying
   * the Gaussian.  This lets "视频" (2 chars, H=1.0, max=1.0 → normalized=4.0)
   * score fairly alongside longer text, without any hard-coded exemption.
   */
  function entropyPenalty(text) {
    const len = text.length;
    if (len <= 1) return 0;
    const h = charEntropy(text);
    // For short text, scale entropy to the [0, ~4.5] range that longer text
    // naturally occupies, so the Gaussian treats them comparably.
    const maxH = Math.log2(len);
    const adjusted = maxH < 4.0 ? (h / maxH) * 4.0 : h;
    const deviation = (adjusted - 4.0) / 1.5;
    return Math.exp(-0.5 * deviation * deviation);
  }

  /**
   * Score a candidate text string for "title quality".
   * Adapted from link_scoring.rs: uses entropy penalty, length factor,
   * density divisor, and letter ratio (replaces IDF which needs DB access).
   *
   *   score = ln(1 + len) × entropyPenalty × letterRatio / ln(e + len)
   *
   * - Noise like "774", "1:02", "R-18" scores low (poor entropy + low letter ratio)
   * - Natural titles like "Burnice - Rising Tempo" score high
   */
  function textScore(text) {
    const t = text.trim();
    if (!t) return 0;
    if (t.startsWith('http://') || t.startsWith('https://')) return 0;
    const len = t.length;
    const lenFactor = Math.log(1 + len);
    const ep = entropyPenalty(t);
    const densityDivisor = Math.log(Math.E + len);
    // Letter ratio: fraction of characters that are letters/CJK (Unicode L category)
    // Replaces IDF as a "naturalness" signal — real titles are mostly letters
    const letterCount = [...t].filter(ch => /\p{L}/u.test(ch)).length;
    const letterRatio = letterCount / len;
    return lenFactor * ep * letterRatio / densityDivisor;
  }

  /**
   * Extract the most meaningful text from a link element.
   *
   * Collects ALL candidate texts (attributes, headings, children, full text)
   * into a single pool, scores each with the entropy + letter-ratio formula,
   * and returns the highest-scoring candidate.  No priority tiers or hard-coded
   * thresholds — the scoring function alone decides what is "best".
   *
   * Returns { text, score } so callers can compare across multiple <a> tags
   * that share the same href.
   */
  function extractLinkText(link) {
    const candidates = new Set();

    // Explicit attributes (aria-label, title) — on link and children
    const ariaLabel = (link.getAttribute('aria-label') || '').trim();
    if (ariaLabel) candidates.add(ariaLabel);
    if (link.title && link.title.trim()) candidates.add(link.title.trim());

    for (const el of link.querySelectorAll('[aria-label]')) {
      const val = (el.getAttribute('aria-label') || '').trim();
      if (val) candidates.add(val);
    }
    for (const el of link.querySelectorAll('[title]')) {
      const val = (el.title || '').trim();
      if (val) candidates.add(val);
    }

    // Semantic heading / strong elements
    for (const el of link.querySelectorAll('h1, h2, h3, h4, h5, h6, strong')) {
      const t = el.textContent.trim();
      if (t) candidates.add(t);
    }

    // Elements with title/name/heading/caption in class or data attributes
    for (const el of link.querySelectorAll(
      '[class*="title"], [class*="name"], [class*="heading"], [class*="caption"], [data-title], [data-name]'
    )) {
      const t = el.textContent.trim();
      if (t) candidates.add(t);
    }

    // Direct child elements
    for (const child of link.children) {
      const t = child.textContent.trim();
      if (t) candidates.add(t);
    }

    // Full textContent
    const full = (link.textContent || '').trim();
    if (full) candidates.add(full);

    // Score every candidate — highest wins
    let bestText = '';
    let bestScore = 0;
    for (const text of candidates) {
      const s = textScore(text);
      if (s > bestScore) {
        bestScore = s;
        bestText = text;
      }
    }

    return { text: bestText, score: bestScore };
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

        const { text: extracted, score } = extractLinkText(link);
        const text = (extracted || href).substring(0, 200);

        const existing = linkMap.get(href);
        if (!existing) {
          linkMap.set(href, { text, url: href, _score: score });
        } else if (score > existing._score) {
          // A higher-scoring <a> for the same URL — upgrade text
          existing.text = text;
          existing._score = score;
        }
      } catch (e) {
        // Skip problematic elements silently
      }
    }

    const result = Array.from(linkMap.values()).map(({ text, url }) => ({ text, url }));
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
