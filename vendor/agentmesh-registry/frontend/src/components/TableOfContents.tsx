import clsx from 'clsx';
import { useTableOfContents } from '../hooks/useTableOfContents';

interface TableOfContentsProps {
  className?: string;
}

export function TableOfContents({ className }: TableOfContentsProps) {
  const { items, activeId } = useTableOfContents('article');

  if (items.length === 0) {
    return null;
  }

  return (
    <nav className={clsx('space-y-2', className)} aria-label="Table of contents">
      <h4 className="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">
        On this page
      </h4>
      <ul className="space-y-1 text-sm">
        {items.map((item) => (
          <li key={item.id}>
            <a
              href={`#${item.id}`}
              className={clsx(
                'block py-1 transition-colors border-l-2 -ml-px',
                item.level === 3 ? 'pl-6' : 'pl-4',
                activeId === item.id
                  ? 'border-accent text-accent font-medium'
                  : 'border-transparent text-text-muted hover:text-text hover:border-border'
              )}
              onClick={(e) => {
                e.preventDefault();
                const element = document.getElementById(item.id);
                if (element) {
                  element.scrollIntoView({ behavior: 'smooth', block: 'start' });
                  // Update URL hash without jumping
                  window.history.pushState(null, '', `#${item.id}`);
                }
              }}
            >
              {item.text}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
