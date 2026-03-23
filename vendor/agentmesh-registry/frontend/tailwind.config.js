/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx,mdx}'],
  darkMode: 'class',
  theme: {
    extend: {
      colors: {
        // Dark theme colors - matching logo aesthetics
        bg: {
          DEFAULT: '#050508',
          surface: '#0a0a12',
          elevated: '#10101a',
          card: '#0d0d15',
        },
        border: {
          DEFAULT: '#1a1a2e',
          hover: '#2a2a4e',
          glow: 'rgba(34, 211, 238, 0.1)',
        },
        text: {
          DEFAULT: '#e4e4eb',
          muted: '#6b7280',
          bright: '#ffffff',
        },
        // Cyan/Teal - from logo glow
        accent: {
          DEFAULT: '#22d3ee',
          hover: '#06b6d4',
          muted: '#0891b2',
          glow: 'rgba(34, 211, 238, 0.25)',
          subtle: 'rgba(34, 211, 238, 0.1)',
        },
        // Gold/Amber - from logo door element (for CTAs)
        gold: {
          DEFAULT: '#f59e0b',
          hover: '#d97706',
          glow: 'rgba(245, 158, 11, 0.25)',
        },
        success: {
          DEFAULT: '#10b981',
          muted: '#059669',
        },
        warning: {
          DEFAULT: '#f59e0b',
        },
        error: {
          DEFAULT: '#ef4444',
        },
      },
      fontFamily: {
        sans: ['Inter', '-apple-system', 'BlinkMacSystemFont', 'sans-serif'],
        mono: ['JetBrains Mono', 'Menlo', 'Monaco', 'monospace'],
      },
      animation: {
        'fade-in': 'fadeIn 0.5s ease-out',
        'fade-in-up': 'fadeInUp 0.6s ease-out',
        'fade-in-down': 'fadeInDown 0.6s ease-out',
        'scale-in': 'scaleIn 0.5s ease-out',
        'glow-pulse': 'glowPulse 3s ease-in-out infinite',
        'float': 'float 6s ease-in-out infinite',
        'mesh-rotate': 'meshRotate 20s linear infinite',
      },
      keyframes: {
        fadeIn: {
          '0%': { opacity: '0' },
          '100%': { opacity: '1' },
        },
        fadeInUp: {
          '0%': { opacity: '0', transform: 'translateY(20px)' },
          '100%': { opacity: '1', transform: 'translateY(0)' },
        },
        fadeInDown: {
          '0%': { opacity: '0', transform: 'translateY(-20px)' },
          '100%': { opacity: '1', transform: 'translateY(0)' },
        },
        scaleIn: {
          '0%': { opacity: '0', transform: 'scale(0.95)' },
          '100%': { opacity: '1', transform: 'scale(1)' },
        },
        glowPulse: {
          '0%, 100%': { opacity: '0.5' },
          '50%': { opacity: '1' },
        },
        float: {
          '0%, 100%': { transform: 'translateY(0)' },
          '50%': { transform: 'translateY(-10px)' },
        },
        meshRotate: {
          '0%': { transform: 'rotate(0deg)' },
          '100%': { transform: 'rotate(360deg)' },
        },
      },
      backgroundImage: {
        'gradient-radial': 'radial-gradient(var(--tw-gradient-stops))',
        'gradient-mesh': 'linear-gradient(135deg, rgba(34, 211, 238, 0.03) 0%, transparent 50%, rgba(245, 158, 11, 0.03) 100%)',
      },
      boxShadow: {
        'glow-cyan': '0 0 40px rgba(34, 211, 238, 0.3)',
        'glow-gold': '0 0 40px rgba(245, 158, 11, 0.3)',
        'glow-cyan-sm': '0 0 20px rgba(34, 211, 238, 0.2)',
      },
      typography: {
        DEFAULT: {
          css: {
            color: '#e4e4eb',
            a: {
              color: '#22d3ee',
              '&:hover': {
                color: '#06b6d4',
              },
            },
            code: {
              color: '#10b981',
              backgroundColor: '#0a0a12',
              padding: '0.125rem 0.25rem',
              borderRadius: '0.25rem',
            },
            'code::before': {
              content: '""',
            },
            'code::after': {
              content: '""',
            },
            pre: {
              backgroundColor: '#0a0a12',
              border: '1px solid #1a1a2e',
            },
            h1: { color: '#e4e4eb' },
            h2: { color: '#e4e4eb' },
            h3: { color: '#e4e4eb' },
            h4: { color: '#e4e4eb' },
            strong: { color: '#e4e4eb' },
            blockquote: {
              color: '#6b7280',
              borderLeftColor: '#22d3ee',
            },
          },
        },
      },
    },
  },
  plugins: [],
};
