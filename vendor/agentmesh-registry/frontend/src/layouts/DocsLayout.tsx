import { useState, useMemo } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';
import clsx from 'clsx';
import { TableOfContents } from '../components';

interface NavSection {
  title: string;
  items: { label: string; href: string }[];
}

const docsNav: NavSection[] = [
  {
    title: 'Getting Started',
    items: [
      { label: 'Introduction', href: '/docs' },
      { label: 'Quick Start - Python', href: '/docs/quickstart-python' },
      { label: 'Quick Start - JavaScript', href: '/docs/quickstart-javascript' },
    ],
  },
  {
    title: 'Core Concepts',
    items: [
      { label: 'Identity & AMID', href: '/docs/concepts/identity' },
      { label: 'KNOCK Protocol', href: '/docs/concepts/knock' },
      { label: 'Encryption', href: '/docs/concepts/encryption' },
      { label: 'Trust Tiers', href: '/docs/concepts/tiers' },
      { label: 'Sessions', href: '/docs/concepts/sessions' },
    ],
  },
  {
    title: 'Python SDK',
    items: [
      { label: 'Installation', href: '/docs/python-sdk/installation' },
      { label: 'Identity', href: '/docs/python-sdk/identity' },
      { label: 'Discovery', href: '/docs/python-sdk/discovery' },
      { label: 'AgentMeshClient', href: '/docs/python-sdk/client' },
      { label: 'Config & Policy', href: '/docs/python-sdk/config' },
      { label: 'Transport', href: '/docs/python-sdk/transport' },
      { label: 'Encryption', href: '/docs/python-sdk/encryption' },
    ],
  },
  {
    title: 'JavaScript SDK',
    items: [
      { label: 'Installation', href: '/docs/javascript-sdk/installation' },
      { label: 'Identity', href: '/docs/javascript-sdk/identity' },
      { label: 'Discovery', href: '/docs/javascript-sdk/discovery' },
      { label: 'Storage', href: '/docs/javascript-sdk/storage' },
      { label: 'Transport', href: '/docs/javascript-sdk/transport' },
      { label: 'Sessions', href: '/docs/javascript-sdk/sessions' },
      { label: 'Encryption', href: '/docs/javascript-sdk/encryption' },
    ],
  },
  {
    title: 'API Reference',
    items: [
      { label: 'Overview', href: '/docs/api/overview' },
      { label: 'Registry Endpoints', href: '/docs/api/registry' },
      { label: 'Authentication', href: '/docs/api/auth' },
    ],
  },
  {
    title: 'Guides',
    items: [
      { label: 'Building Your First Agent', href: '/docs/guides/first-agent' },
      { label: 'Receiving Messages', href: '/docs/guides/receiving-messages' },
      { label: 'Security Best Practices', href: '/docs/guides/security' },
      { label: 'Deployment', href: '/docs/guides/deployment' },
    ],
  },
];

// Flatten navigation for prev/next
function flattenNav(nav: NavSection[]): { label: string; href: string }[] {
  return nav.flatMap(section => section.items);
}

// GitHub base URL for edit links
const GITHUB_EDIT_BASE = 'https://github.com/amitayks/agentmesh/edit/main/registry/frontend/src/pages/DocsPage.tsx';

export function DocsLayout() {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const location = useLocation();

  const flatNav = useMemo(() => flattenNav(docsNav), []);

  // Find current page index and prev/next pages
  const currentIndex = flatNav.findIndex(item => {
    if (item.href === '/docs') {
      return location.pathname === '/docs';
    }
    return location.pathname === item.href;
  });

  const prevPage = currentIndex > 0 ? flatNav[currentIndex - 1] : null;
  const nextPage = currentIndex < flatNav.length - 1 ? flatNav[currentIndex + 1] : null;

  const isActive = (href: string) => {
    if (href === '/docs') {
      return location.pathname === '/docs';
    }
    return location.pathname.startsWith(href);
  };

  return (
    <div className="flex min-h-[calc(100vh-4rem)]">
      {/* Mobile sidebar toggle */}
      <button
        type="button"
        className="fixed bottom-4 right-4 z-50 md:hidden bg-accent text-white p-3 rounded-full shadow-lg"
        onClick={() => setSidebarOpen(!sidebarOpen)}
        aria-label="Toggle navigation"
      >
        <svg className="w-6 h-6" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M4 6h16M4 12h16M4 18h16"
          />
        </svg>
      </button>

      {/* Sidebar */}
      <aside
        className={clsx(
          'fixed inset-y-0 left-0 z-40 w-72 bg-bg border-r border-border overflow-y-auto',
          'transform transition-transform duration-200 ease-out',
          'md:relative md:translate-x-0 md:pt-0 pt-16',
          sidebarOpen ? 'translate-x-0' : '-translate-x-full'
        )}
      >
        <nav className="p-4 space-y-6">
          {docsNav.map((section) => (
            <div key={section.title}>
              <h3 className="text-xs font-semibold text-text-muted uppercase tracking-wider mb-2 px-2">
                {section.title}
              </h3>
              <ul className="space-y-1">
                {section.items.map((item) => (
                  <li key={item.href}>
                    <Link
                      to={item.href}
                      className={clsx(
                        'block px-3 py-2 rounded-lg text-sm transition-colors',
                        isActive(item.href)
                          ? 'bg-accent/10 text-accent font-medium'
                          : 'text-text-muted hover:text-text hover:bg-bg-surface'
                      )}
                      onClick={() => setSidebarOpen(false)}
                    >
                      {item.label}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>
      </aside>

      {/* Backdrop for mobile */}
      {sidebarOpen && (
        <div
          className="fixed inset-0 bg-black/50 z-30 md:hidden"
          onClick={() => setSidebarOpen(false)}
        />
      )}

      {/* Main content */}
      <main className="flex-1 min-w-0 px-4 sm:px-6 lg:px-8 py-8 max-w-4xl">
        <Outlet />

        {/* Edit on GitHub link */}
        <div className="mt-12 pt-6 border-t border-border">
          <a
            href={GITHUB_EDIT_BASE}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-2 text-sm text-text-muted hover:text-accent transition-colors"
          >
            <svg className="w-4 h-4" fill="currentColor" viewBox="0 0 24 24">
              <path fillRule="evenodd" d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z" clipRule="evenodd" />
            </svg>
            Edit this page on GitHub
          </a>
        </div>

        {/* Previous/Next navigation */}
        <nav className="mt-8 flex items-center justify-between gap-4">
          {prevPage ? (
            <Link
              to={prevPage.href}
              className="group flex flex-col items-start gap-1 px-4 py-3 rounded-lg border border-border hover:border-accent/50 hover:bg-bg-surface/50 transition-colors flex-1 max-w-xs"
            >
              <span className="text-xs text-text-muted group-hover:text-accent transition-colors flex items-center gap-1">
                <svg className="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 19l-7-7 7-7" />
                </svg>
                Previous
              </span>
              <span className="text-sm font-medium text-text group-hover:text-accent transition-colors">
                {prevPage.label}
              </span>
            </Link>
          ) : (
            <div className="flex-1 max-w-xs" />
          )}

          {nextPage ? (
            <Link
              to={nextPage.href}
              className="group flex flex-col items-end gap-1 px-4 py-3 rounded-lg border border-border hover:border-accent/50 hover:bg-bg-surface/50 transition-colors flex-1 max-w-xs"
            >
              <span className="text-xs text-text-muted group-hover:text-accent transition-colors flex items-center gap-1">
                Next
                <svg className="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
                </svg>
              </span>
              <span className="text-sm font-medium text-text group-hover:text-accent transition-colors">
                {nextPage.label}
              </span>
            </Link>
          ) : (
            <div className="flex-1 max-w-xs" />
          )}
        </nav>
      </main>

      {/* Right sidebar - Table of Contents (desktop only) */}
      <aside className="hidden xl:block w-56 shrink-0 py-8 pr-4">
        <div className="sticky top-20">
          <TableOfContents />
        </div>
      </aside>
    </div>
  );
}
