import { useState, useEffect } from 'react';
import clsx from 'clsx';

interface CodeBlockProps {
  code: string;
  language: string;
  filename?: string;
  showLineNumbers?: boolean;
}

// Simple syntax highlighting colors
const languageColors: Record<string, string> = {
  python: '#3776ab',
  typescript: '#3178c6',
  javascript: '#f7df1e',
  bash: '#4eaa25',
  json: '#292929',
  rust: '#dea584',
};

// Basic token patterns for syntax highlighting
function tokenize(code: string, language: string): React.ReactNode[] {
  const lines = code.split('\n');

  return lines.map((line, i) => (
    <div key={i} className="flex">
      <span className="text-right pr-4 text-text-muted select-none w-8 shrink-0">
        {i + 1}
      </span>
      <span className="flex-1">{highlightLine(line, language)}</span>
    </div>
  ));
}

function highlightLine(line: string, language: string): React.ReactNode {
  // Simple keyword-based highlighting
  const patterns: Record<string, { pattern: RegExp; className: string }[]> = {
    python: [
      { pattern: /(#.*)$/gm, className: 'text-text-muted' },
      { pattern: /\b(import|from|async|await|def|class|if|else|return|True|False|None)\b/g, className: 'text-purple-400' },
      { pattern: /(['"`])((?:(?!\1)[^\\]|\\.)*)(\1)/g, className: 'text-success' },
      { pattern: /\b(Identity|RegistryClient|Transport|AgentMeshClient)\b/g, className: 'text-blue-400' },
    ],
    typescript: [
      { pattern: /(\/\/.*)$/gm, className: 'text-text-muted' },
      { pattern: /\b(import|from|export|const|let|var|async|await|function|class|if|else|return|new|type|interface)\b/g, className: 'text-purple-400' },
      { pattern: /(['"`])((?:(?!\1)[^\\]|\\.)*)(\1)/g, className: 'text-success' },
      { pattern: /\b(Identity|RegistryClient|P2PTransport|AgentMeshClient)\b/g, className: 'text-blue-400' },
    ],
    javascript: [
      { pattern: /(\/\/.*)$/gm, className: 'text-text-muted' },
      { pattern: /\b(import|from|export|const|let|var|async|await|function|class|if|else|return|new)\b/g, className: 'text-purple-400' },
      { pattern: /(['"`])((?:(?!\1)[^\\]|\\.)*)(\1)/g, className: 'text-success' },
    ],
    bash: [
      { pattern: /(#.*)$/gm, className: 'text-text-muted' },
      { pattern: /\b(npm|pip|install|cd|mkdir)\b/g, className: 'text-purple-400' },
    ],
    json: [
      { pattern: /("(?:[^"\\]|\\.)*")(\s*:)/g, className: 'text-blue-400' },
      { pattern: /:\s*("(?:[^"\\]|\\.)*")/g, className: 'text-success' },
      { pattern: /:\s*(\d+)/g, className: 'text-orange-400' },
      { pattern: /:\s*(true|false|null)/g, className: 'text-purple-400' },
    ],
  };

  const langPatterns = patterns[language] || [];

  // For simplicity, return line with basic escaping
  // In production, use a proper syntax highlighter like Shiki
  let result = line;

  // Escape HTML
  result = result.replace(/</g, '&lt;').replace(/>/g, '&gt;');

  // Apply highlighting patterns
  for (const { pattern, className } of langPatterns) {
    result = result.replace(pattern, (match) => {
      return `<span class="${className}">${match}</span>`;
    });
  }

  return <span dangerouslySetInnerHTML={{ __html: result }} />;
}

export function CodeBlock({ code, language, filename, showLineNumbers = true }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    await navigator.clipboard.writeText(code);
    setCopied(true);
  };

  useEffect(() => {
    if (copied) {
      const timer = setTimeout(() => setCopied(false), 2000);
      return () => clearTimeout(timer);
    }
  }, [copied]);

  const langColor = languageColors[language] || '#888';

  return (
    <div className="code-block">
      {/* Header */}
      <div className="code-header">
        <div className="flex items-center gap-2">
          <span
            className="w-2 h-2 rounded-full"
            style={{ backgroundColor: langColor }}
          />
          <span className="text-xs text-text-muted uppercase font-medium">
            {filename || language}
          </span>
        </div>
        <button
          onClick={handleCopy}
          className={clsx(
            'text-xs px-2 py-1 rounded transition-all',
            copied
              ? 'bg-success/20 text-success'
              : 'bg-bg-surface hover:bg-bg-elevated text-text-muted hover:text-text'
          )}
          aria-label={copied ? 'Copied!' : 'Copy code'}
          aria-live="polite"
        >
          {copied ? 'Copied!' : 'Copy'}
        </button>
      </div>

      {/* Code content */}
      <div className="code-content">
        <pre className="font-mono text-sm leading-relaxed">
          {showLineNumbers ? (
            tokenize(code, language)
          ) : (
            <code>{highlightLine(code, language)}</code>
          )}
        </pre>
      </div>
    </div>
  );
}
