import { useState, useEffect, useCallback } from 'react';

export interface TocItem {
  id: string;
  text: string;
  level: number;
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/(^-|-$)/g, '');
}

export function useTableOfContents(contentSelector: string = 'article') {
  const [items, setItems] = useState<TocItem[]>([]);
  const [activeId, setActiveId] = useState<string>('');

  // Scan for headings and build ToC
  const scanHeadings = useCallback(() => {
    const content = document.querySelector(contentSelector);
    if (!content) return;

    const headings = content.querySelectorAll('h2, h3');
    const tocItems: TocItem[] = [];

    headings.forEach((heading) => {
      const text = heading.textContent || '';
      let id = heading.id;

      // Generate ID if not present
      if (!id) {
        id = slugify(text);
        // Ensure uniqueness
        let counter = 1;
        let uniqueId = id;
        while (document.getElementById(uniqueId)) {
          uniqueId = `${id}-${counter}`;
          counter++;
        }
        heading.id = uniqueId;
        id = uniqueId;
      }

      tocItems.push({
        id,
        text,
        level: heading.tagName === 'H2' ? 2 : 3,
      });
    });

    setItems(tocItems);
  }, [contentSelector]);

  // Track active heading with Intersection Observer
  useEffect(() => {
    scanHeadings();

    // Re-scan when content might have changed
    const observer = new MutationObserver(() => {
      scanHeadings();
    });

    const content = document.querySelector(contentSelector);
    if (content) {
      observer.observe(content, { childList: true, subtree: true });
    }

    return () => observer.disconnect();
  }, [contentSelector, scanHeadings]);

  // Intersection Observer for active tracking
  useEffect(() => {
    if (items.length === 0) return;

    const observerCallback: IntersectionObserverCallback = (entries) => {
      // Find the first heading that's in view or just above the viewport
      const visibleEntries = entries.filter((entry) => entry.isIntersecting);

      if (visibleEntries.length > 0) {
        // Sort by position and take the first one
        const sorted = visibleEntries.sort((a, b) => {
          const rectA = a.boundingClientRect;
          const rectB = b.boundingClientRect;
          return rectA.top - rectB.top;
        });
        setActiveId(sorted[0].target.id);
      }
    };

    const observer = new IntersectionObserver(observerCallback, {
      rootMargin: '-80px 0px -80% 0px',
      threshold: 0,
    });

    items.forEach((item) => {
      const element = document.getElementById(item.id);
      if (element) {
        observer.observe(element);
      }
    });

    return () => observer.disconnect();
  }, [items]);

  return { items, activeId };
}
