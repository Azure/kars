import { useState } from 'react';
import { Link, useLocation } from 'react-router-dom';
import clsx from 'clsx';
import agentmeshLogo from '../content/agentmesh.png';

const navItems = [
  { label: 'Docs', href: '/docs' },
  { label: 'GitHub', href: 'https://github.com/amitayks/agentmesh', external: true },
  { label: 'npm', href: 'https://www.npmjs.com/package/@agentmesh/sdk', external: true },
];

export function Header() {
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const location = useLocation();

  const isActive = (href: string) => {
    if (href === '/docs') {
      return location.pathname.startsWith('/docs');
    }
    return location.pathname === href;
  };

  return (
    <header className="sticky top-0 z-40 bg-bg/80 backdrop-blur-lg border-b border-border">
      <nav className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        <div className="flex items-center justify-between h-16">
          {/* Logo */}
          <Link
            to="/"
            className="flex items-center gap-2.5 text-text-bright font-semibold hover:opacity-80 transition-opacity"
          >
            <img
              src={agentmeshLogo}
              alt=""
              className="w-7 h-7 object-contain"
              width={28}
              height={28}
              aria-hidden="true"
            />
            <span className="text-sm font-bold tracking-wide">
              <span className="text-accent">Agent</span>
              <span className="text-text-bright">Mesh</span>
            </span>
          </Link>

          {/* Desktop Navigation */}
          <div className="hidden md:flex items-center gap-6">
            {navItems.map((item) =>
              item.external ? (
                <a
                  key={item.label}
                  href={item.href}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-text-muted hover:text-text transition-colors text-sm"
                >
                  {item.label}
                </a>
              ) : (
                <Link
                  key={item.label}
                  to={item.href}
                  className={clsx(
                    'text-sm transition-colors',
                    isActive(item.href)
                      ? 'text-accent font-medium'
                      : 'text-text-muted hover:text-text'
                  )}
                >
                  {item.label}
                </Link>
              )
            )}
            <a
              href="/v1/health"
              className="badge badge-success text-xs"
              target="_blank"
              rel="noopener noreferrer"
            >
              API Status
            </a>
          </div>

          {/* Mobile menu button */}
          <button
            type="button"
            className="md:hidden p-2 text-text-muted hover:text-text transition-colors"
            onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
            aria-expanded={mobileMenuOpen}
            aria-label="Toggle navigation menu"
          >
            <svg
              className="w-6 h-6"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              {mobileMenuOpen ? (
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M6 18L18 6M6 6l12 12"
                />
              ) : (
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M4 6h16M4 12h16M4 18h16"
                />
              )}
            </svg>
          </button>
        </div>

        {/* Mobile Navigation */}
        {mobileMenuOpen && (
          <div className="md:hidden py-4 border-t border-border animate-fade-in">
            <div className="flex flex-col gap-4">
              {navItems.map((item) =>
                item.external ? (
                  <a
                    key={item.label}
                    href={item.href}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-text-muted hover:text-text transition-colors px-2 py-1"
                    onClick={() => setMobileMenuOpen(false)}
                  >
                    {item.label}
                  </a>
                ) : (
                  <Link
                    key={item.label}
                    to={item.href}
                    className={clsx(
                      'transition-colors px-2 py-1',
                      isActive(item.href)
                        ? 'text-accent font-medium'
                        : 'text-text-muted hover:text-text'
                    )}
                    onClick={() => setMobileMenuOpen(false)}
                  >
                    {item.label}
                  </Link>
                )
              )}
              <a
                href="/v1/health"
                className="badge badge-success text-xs w-fit"
                target="_blank"
                rel="noopener noreferrer"
              >
                API Status
              </a>
            </div>
          </div>
        )}
      </nav>
    </header>
  );
}
