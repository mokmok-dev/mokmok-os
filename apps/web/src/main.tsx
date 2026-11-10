import '@cloudflare/kumo/styles/standalone';
import './styles.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { applyTheme, readTheme } from './theme';

applyTheme(readTheme(window.localStorage));

const container = document.getElementById('root');
if (container === null) {
  throw new Error('the page has no root element');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
