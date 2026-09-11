import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: {
    default: "kars Bridge",
    template: "%s · kars Bridge",
  },
  description:
    "Harness-neutral agent mission control on the kars secure-agent substrate.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={`${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <head>
        {/* Apply the saved theme choice before first paint so there's no flash
            of the wrong palette. No stored choice = follow the OS preference. */}
        <script
          dangerouslySetInnerHTML={{
            __html:
              "try{var t=localStorage.getItem('kb-theme');if(t==='dark')document.documentElement.classList.add('dark');else if(t==='light')document.documentElement.classList.add('light');}catch(e){}",
          }}
        />
      </head>
      <body className="min-h-full font-sans">
        {/* B35 — skip-navigation link for keyboard users (first focusable
            element; visually hidden until focused). Section layouts mark their
            main region with id="main-content". */}
        <a
          href="#main-content"
          className="sr-only left-2 top-2 z-50 rounded-lg bg-signal px-3 py-2 text-sm font-medium text-signal-fg focus:not-sr-only focus:absolute"
        >
          Skip to main content
        </a>
        {children}
      </body>
    </html>
  );
}
