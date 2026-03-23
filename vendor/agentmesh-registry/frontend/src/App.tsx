import { Routes, Route } from 'react-router-dom';
import { Header } from './components/Header';
import { Footer } from './components/Footer';
import { LandingPage } from './pages/LandingPage';
import { DocsLayout } from './layouts/DocsLayout';
import { DocsPage } from './pages/DocsPage';

export default function App() {
  return (
    <div className="min-h-screen flex flex-col">
      <a href="#main-content" className="skip-link">
        Skip to main content
      </a>
      <Header />
      <main id="main-content" className="flex-1">
        <Routes>
          <Route path="/" element={<LandingPage />} />
          <Route path="/docs" element={<DocsLayout />}>
            <Route index element={<DocsPage slug="introduction" />} />
            <Route path=":slug" element={<DocsPage />} />
            <Route path=":category/:slug" element={<DocsPage />} />
          </Route>
        </Routes>
      </main>
      <Footer />
    </div>
  );
}
