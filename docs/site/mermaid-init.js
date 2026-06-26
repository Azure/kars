// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

(() => {
    const darkThemes = ['ayu', 'navy', 'coal'];
    const lightThemes = ['light', 'rust'];

    const classList = document.getElementsByTagName('html')[0].classList;

    let lastThemeWasLight = true;
    for (const cssClass of classList) {
        if (darkThemes.includes(cssClass)) {
            lastThemeWasLight = false;
            break;
        }
    }

    const theme = lastThemeWasLight ? 'base' : 'dark';

    // kars brand-aligned Mermaid palette. Using the `base` theme on light
    // surfaces lets us drive every colour from Azure-family tokens; the
    // built-in `dark` theme already reads well on coal/navy/ayu.
    const fontFamily =
        '"Inter", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';

    const themeVariables = lastThemeWasLight
        ? {
            fontFamily,
            primaryColor: '#e8f3fc',
            primaryBorderColor: '#0078d4',
            primaryTextColor: '#0b1220',
            lineColor: '#5b6b7b',
            secondaryColor: '#f3edfb',
            secondaryBorderColor: '#8a63d2',
            tertiaryColor: '#f5f7fb',
            tertiaryBorderColor: '#c5cedb',
            clusterBkg: '#f5f7fb',
            clusterBorder: '#c5cedb',
        }
        : {
            fontFamily,
            primaryColor: '#1b2030',
            primaryBorderColor: '#2899f5',
            primaryTextColor: '#eef2f8',
            lineColor: '#8aa0b6',
            secondaryColor: '#241b33',
            secondaryBorderColor: '#8a63d2',
            tertiaryColor: '#161a27',
            tertiaryBorderColor: '#3a4356',
            clusterBkg: '#161a27',
            clusterBorder: '#3a4356',
        };

    mermaid.initialize({
        startOnLoad: true,
        theme,
        themeVariables,
        securityLevel: 'strict',
        flowchart: { curve: 'basis', htmlLabels: true, wrappingWidth: 460 },
        themeCSS: '.label, .nodeLabel, .edgeLabel { font-family: ' + fontFamily + '; }',
    });

    // Simplest way to make mermaid re-render the diagrams in the new theme is via refreshing the page

    for (const darkTheme of darkThemes) {
        const el = document.getElementById(darkTheme);
        if (!el) continue;
        el.addEventListener('click', () => {
            if (lastThemeWasLight) {
                window.location.reload();
            }
        });
    }

    for (const lightTheme of lightThemes) {
        const el = document.getElementById(lightTheme);
        if (!el) continue;
        el.addEventListener('click', () => {
            if (!lastThemeWasLight) {
                window.location.reload();
            }
        });
    }
})();
