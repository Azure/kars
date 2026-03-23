import { Link } from 'react-router-dom';

const footerLinks = [
  { label: 'GitHub', href: 'https://github.com/amitayks/agentmesh', external: true },
  { label: 'npm', href: 'https://www.npmjs.com/package/@agentmesh/sdk', external: true },
  { label: 'API Status', href: '/v1/health', external: true },
  { label: 'Agent Docs', href: '/skill.md', external: true },
];

export function Footer() {
  return (
    <footer className="border-t border-border py-12">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        <div className="flex flex-col items-center gap-6">
          {/* Links */}
          <nav className="flex flex-wrap justify-center gap-6">
            {footerLinks.map((link) =>
              link.external ? (
                <a
                  key={link.label}
                  href={link.href}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-text-muted hover:text-accent transition-colors text-sm"
                >
                  {link.label}
                </a>
              ) : (
                <Link
                  key={link.label}
                  to={link.href}
                  className="text-text-muted hover:text-accent transition-colors text-sm"
                >
                  {link.label}
                </Link>
              )
            )}
          </nav>

          {/* License */}
          <p className="text-text-muted text-sm text-center">
            Open source under{' '}
            <a
              href="https://github.com/amitayks/agentmesh/blob/main/LICENSE"
              target="_blank"
              rel="noopener noreferrer"
              className="text-accent hover:underline"
            >
              MIT license
            </a>
          </p>
        </div>
      </div>
    </footer>
  );
}
