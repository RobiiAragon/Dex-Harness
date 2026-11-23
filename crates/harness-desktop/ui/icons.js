// Centralized monochrome SVG icon registry (Lucide / SF Symbols style)
(function (global) {
  'use strict';

  const PATHS = {
    dexLogo:
      '<path d="M12 2.5L20.5 7.4v9.2L12 21.5 3.5 16.6V7.4L12 2.5z" />' +
      '<path d="M12 7.2l4.2 2.4v4.8L12 16.8l-4.2-2.4V9.6L12 7.2z" />' +
      '<circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />',
    terminal:
      '<polyline points="4 17 10 11 4 5" /><line x1="12" y1="19" x2="20" y2="19" />',
    bot:
      '<rect x="3" y="11" width="18" height="10" rx="2" /><circle cx="12" cy="5" r="2" />' +
      '<path d="M12 7v4" /><line x1="8" y1="16" x2="8" y2="16.01" /><line x1="16" y1="16" x2="16" y2="16.01" />',
    sparkles:
      '<path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3z" />' +
      '<path d="M19 3v4" /><path d="M17 5h4" />',
    image:
      '<rect x="3" y="3" width="18" height="18" rx="2" /><circle cx="8.5" cy="8.5" r="1.5" />' +
      '<polyline points="21 15 16 10 5 21" />',
    mic:
      '<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" />' +
      '<path d="M19 10v2a7 7 0 0 1-14 0v-2" /><line x1="12" y1="19" x2="12" y2="22" />',
    micOff:
      '<line x1="2" y1="2" x2="22" y2="22" /><path d="M18.89 13.23A7.12 7.12 0 0 0 19 12v-2" />' +
      '<path d="M5 10v2a7 7 0 0 0 12 5" /><path d="M15 9.34V5a3 3 0 0 0-5.68-1.33" />' +
      '<path d="M9 9v3a3 3 0 0 0 5.12 2.12" /><line x1="12" y1="19" x2="12" y2="22" />',
    cube:
      '<path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />' +
      '<polyline points="3.27 6.96 12 12.01 20.73 6.96" /><line x1="12" y1="22.08" x2="12" y2="12" />',
    cpu:
      '<rect x="4" y="4" width="16" height="16" rx="2" /><rect x="9" y="9" width="6" height="6" />' +
      '<path d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 14h3M1 9h3M1 14h3" />',
    puzzle:
      '<path d="M19.439 7.85c-.049.322.059.648.289.878l1.568 1.568c.47.47.706 1.087.706 1.704s-.235 1.233-.706 1.704l-1.611 1.611a.98.98 0 0 1-.837.276c-.47-.07-.802-.48-.968-.925a2.501 2.501 0 1 0-3.214 3.214c.446.166.855.497.925.968a.979.979 0 0 1-.276.837l-1.61 1.61a2.404 2.404 0 0 1-1.705.707 2.402 2.402 0 0 1-1.704-.706l-1.568-1.568a1.026 1.026 0 0 0-.877-.29c-.493.074-.84.504-1.02.968a2.5 2.5 0 1 1-3.237-3.237c.464-.18.894-.527.967-1.02a1.026 1.026 0 0 0-.289-.877l-1.568-1.568A2.402 2.402 0 0 1 1.998 12c0-.617.236-1.234.706-1.704L4.23 8.77c.24-.24.581-.353.917-.303.515.077.877.528 1.073 1.01a2.5 2.5 0 1 0 3.259-3.259c-.482-.196-.933-.558-1.01-1.073-.05-.336.062-.676.303-.917l1.525-1.525A2.402 2.402 0 0 1 12 1.998c.617 0 1.234.236 1.704.706l1.568 1.568c.23.23.556.338.877.29.493-.074.84-.504 1.02-.968a2.5 2.5 0 1 1 3.237 3.237c-.464.18-.894.527-.967 1.02z" />',
    gauge:
      '<path d="M12 14l4-4" /><path d="M3.34 19a10 10 0 1 1 17.32 0" />',
    panelLeft:
      '<rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />',
    panelRight:
      '<rect x="3" y="3" width="18" height="18" rx="2" /><line x1="15" y1="3" x2="15" y2="21" />',
    send:
      '<line x1="12" y1="19" x2="12" y2="5" /><polyline points="5 12 12 5 19 12" />',
    square:
      '<rect x="5" y="5" width="14" height="14" rx="2" fill="currentColor" stroke="none" />',
    play:
      '<polygon points="6 4 20 12 6 20 6 4" />',
    refresh:
      '<path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />' +
      '<path d="M3 3v5h5" /><path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />' +
      '<path d="M16 16h5v5" />',
    trash:
      '<polyline points="3 6 5 6 21 6" /><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />' +
      '<path d="M10 11v6" /><path d="M14 11v6" /><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />',
    folder:
      '<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />',
    search:
      '<circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" />',
    download:
      '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />' +
      '<polyline points="7 10 12 15 17 10" /><line x1="12" y1="15" x2="12" y2="3" />',
    upload:
      '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />' +
      '<polyline points="17 8 12 3 7 8" /><line x1="12" y1="3" x2="12" y2="15" />',
    plus:
      '<line x1="12" y1="5" x2="12" y2="19" /><line x1="5" y1="12" x2="19" y2="12" />',
    x:
      '<line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />',
    check:
      '<polyline points="20 6 9 17 4 12" />',
    checkCircle:
      '<path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" /><polyline points="22 4 12 14.01 9 11.01" />',
    xCircle:
      '<circle cx="12" cy="12" r="10" /><line x1="15" y1="9" x2="9" y2="15" /><line x1="9" y1="9" x2="15" y2="15" />',
    alertTriangle:
      '<path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />' +
      '<line x1="12" y1="9" x2="12" y2="13" /><line x1="12" y1="17" x2="12.01" y2="17" />',
    shield:
      '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />',
    sliders:
      '<line x1="4" y1="21" x2="4" y2="14" /><line x1="4" y1="10" x2="4" y2="3" />' +
      '<line x1="12" y1="21" x2="12" y2="12" /><line x1="12" y1="8" x2="12" y2="3" />' +
      '<line x1="20" y1="21" x2="20" y2="16" /><line x1="20" y1="12" x2="20" y2="3" />' +
      '<line x1="1" y1="14" x2="7" y2="14" /><line x1="9" y1="8" x2="15" y2="8" /><line x1="17" y1="16" x2="23" y2="16" />',
    chevronDown:
      '<polyline points="6 9 12 15 18 9" />',
    chevronRight:
      '<polyline points="9 18 15 12 9 6" />',
    eye:
      '<path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" />',
    fileText:
      '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />' +
      '<polyline points="14 2 14 8 20 8" /><line x1="16" y1="13" x2="8" y2="13" /><line x1="16" y1="17" x2="8" y2="17" />',
    scan:
      '<path d="M3 7V5a2 2 0 0 1 2-2h2" /><path d="M17 3h2a2 2 0 0 1 2 2v2" />' +
      '<path d="M21 17v2a2 2 0 0 1-2 2h-2" /><path d="M7 21H5a2 2 0 0 1-2-2v-2" />' +
      '<line x1="7" y1="12" x2="17" y2="12" />',
    volume:
      '<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />' +
      '<path d="M15.54 8.46a5 5 0 0 1 0 7.07" /><path d="M19.07 4.93a10 10 0 0 1 0 14.14" />',
    film:
      '<rect x="2" y="2" width="20" height="20" rx="2.18" />' +
      '<line x1="7" y1="2" x2="7" y2="22" /><line x1="17" y1="2" x2="17" y2="22" />' +
      '<line x1="2" y1="12" x2="22" y2="12" /><line x1="2" y1="7" x2="7" y2="7" />' +
      '<line x1="2" y1="17" x2="7" y2="17" /><line x1="17" y1="17" x2="22" y2="17" />' +
      '<line x1="17" y1="7" x2="22" y2="7" />',
    grid:
      '<rect x="3" y="3" width="7" height="7" /><rect x="14" y="3" width="7" height="7" />' +
      '<rect x="14" y="14" width="7" height="7" /><rect x="3" y="14" width="7" height="7" />',
    wireframe:
      '<polygon points="12 2 2 7 12 12 22 7 12 2" /><polyline points="2 17 12 22 22 17" />' +
      '<polyline points="2 12 12 17 22 12" />',
    rotateCcw:
      '<polyline points="1 4 1 10 7 10" /><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" />',
    gamepad:
      '<line x1="6" y1="12" x2="10" y2="12" /><line x1="8" y1="10" x2="8" y2="14" />' +
      '<line x1="15" y1="13" x2="15.01" y2="13" /><line x1="18" y1="11" x2="18.01" y2="11" />' +
      '<rect x="2" y="6" width="20" height="12" rx="4" />',
    layers:
      '<polygon points="12 2 2 7 12 12 22 7 12 2" /><polyline points="2 12 12 17 22 12" />',
    wrench:
      '<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />',
    zap:
      '<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />',
    activity:
      '<polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />',
    arrowUpRight:
      '<line x1="7" y1="17" x2="17" y2="7" /><polyline points="7 7 17 7 17 17" />',
    clock:
      '<circle cx="12" cy="12" r="10" /><polyline points="12 6 12 12 16 14" />',
    maximize:
      '<polyline points="15 3 21 3 21 9" /><polyline points="9 21 3 21 3 15" />' +
      '<line x1="21" y1="3" x2="14" y2="10" /><line x1="3" y1="21" x2="10" y2="14" />',
    camera:
      '<path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" />' +
      '<circle cx="12" cy="13" r="4" />',
    listChecks:
      '<path d="M3 17l2 2 4-4" /><path d="M3 7l2 2 4-4" />' +
      '<path d="M13 6h8" /><path d="M13 12h8" /><path d="M13 18h8" />',
    gitBranch:
      '<line x1="6" y1="3" x2="6" y2="15" /><circle cx="18" cy="6" r="3" />' +
      '<circle cx="6" cy="18" r="3" /><path d="M18 9a9 9 0 0 1-9 9" />',
    externalLink:
      '<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />' +
      '<polyline points="15 3 21 3 21 9" /><line x1="10" y1="14" x2="21" y2="3" />',
    folderPlus:
      '<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />' +
      '<line x1="12" y1="11" x2="12" y2="17" /><line x1="9" y1="14" x2="15" y2="14" />',
    brain:
      '<path d="M9.5 2A2.5 2.5 0 0 1 12 4.5v15a2.5 2.5 0 0 1-4.96.44 2.5 2.5 0 0 1-2.96-3.08 3 3 0 0 1-.34-5.58 2.5 2.5 0 0 1 1.32-4.24 2.5 2.5 0 0 1 4.44-1.54z" />' +
      '<path d="M14.5 2A2.5 2.5 0 0 0 12 4.5v15a2.5 2.5 0 0 0 4.96.44 2.5 2.5 0 0 0 2.96-3.08 3 3 0 0 0 .34-5.58 2.5 2.5 0 0 0-1.32-4.24 2.5 2.5 0 0 0-4.44-1.54z" />',
    server:
      '<rect x="2" y="2" width="20" height="8" rx="2" /><rect x="2" y="14" width="20" height="8" rx="2" />' +
      '<line x1="6" y1="6" x2="6.01" y2="6" /><line x1="6" y1="18" x2="6.01" y2="18" />',
    paperclip:
      '<path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />',
    folderOpen:
      '<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />' +
      '<path d="M2 10h20" />',
    messageSquarePlus:
      '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />' +
      '<line x1="12" y1="7" x2="12" y2="13" /><line x1="9" y1="10" x2="15" y2="10" />',
    power:
      '<path d="M18.36 6.64a9 9 0 1 1-12.73 0" /><line x1="12" y1="2" x2="12" y2="12" />',
    code:
      '<polyline points="16 18 22 12 16 6" /><polyline points="8 6 2 12 8 18" />',
    file:
      '<path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />' +
      '<polyline points="13 2 13 9 20 9" />',
    edit:
      '<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />' +
      '<path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />',
    copy:
      '<rect x="9" y="9" width="13" height="13" rx="2" />' +
      '<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />',
    globe:
      '<circle cx="12" cy="12" r="10" /><line x1="2" y1="12" x2="22" y2="12" />' +
      '<path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />',
    headphones:
      '<path d="M3 18v-6a9 9 0 0 1 18 0v6" />' +
      '<path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3zM3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z" />',
    user:
      '<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" /><circle cx="12" cy="7" r="4" />',
  };

  function svg(name, size, extraClass) {
    const px = size || 16;
    const cls = extraClass ? ` class="icon ${extraClass}"` : ' class="icon"';
    const body = PATHS[name] || PATHS.terminal;
    return `<svg${cls} width="${px}" height="${px}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
  }

  function hydrate(root) {
    const scope = root || document;
    const nodes = scope.querySelectorAll('[data-icon]');
    for (let i = 0; i < nodes.length; i++) {
      const el = nodes[i];
      const name = el.getAttribute('data-icon');
      const size = Number(el.getAttribute('data-icon-size')) || 16;
      const cls = el.getAttribute('data-icon-class') || '';
      el.innerHTML = svg(name, size, cls);
    }
  }

  global.DexIcons = { svg, hydrate, PATHS };
})(window);
