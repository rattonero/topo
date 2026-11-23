// Rattonero pagination script.
//
// The front end injects this into every chapter as a privileged user script
// (book content itself runs no script at all). It lays the chapter out in
// pages, turns them, and reports what the reader does.
//
// Front end -> page (call as JavaScript):
//   Rattonero.configure(settings, start)  before content loads
//   Rattonero.apply(settings)             settings changed
//   Rattonero.next() / Rattonero.prev()   turn the page
//   Rattonero.goToFraction(f) / Rattonero.goToAnchor(id)
//     settings = { theme: "light"|"sepia"|"dark", fontSize: px,
//                  lineHeight: number, margin: px }
//     start    = { fraction: 0..1 } | { anchor: "id" } | { end: true }
//
// Page -> front end (JSON messages on the "rattonero" channel):
//   { type: "position", page, pages }   after every layout or page turn
//   { type: "edge", direction: "next"|"prev" }  turned past the chapter
//   { type: "link", href }              a link was activated
//   { type: "image", src }              an image was activated (zoom)
//   { type: "tap" }                     tap in the middle (toggle controls)
//   { type: "selection", text, rect }   text selection changed (word lookup)
(function () {
  'use strict';
  if (window.Rattonero) return;

  var root = document.documentElement;
  var R = { page: 0, pages: 1 };
  var start = null;
  var ready = false;
  var fraction = 0;

  function post(message) {
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.rattonero) {
        window.webkit.messageHandlers.rattonero.postMessage(message);   // WebKit (Apple, GTK)
      } else if (window.chrome && window.chrome.webview) {
        window.chrome.webview.postMessage(message);                      // WebView2
      } else if (window.rattoneroHost && window.rattoneroHost.postMessage) {
        window.rattoneroHost.postMessage(JSON.stringify(message));       // anything else
      }
    } catch (e) { /* no host listening */ }
  }

  function pageWidth() { return window.innerWidth || root.clientWidth || 1; }

  function place(offset, animated) {
    var body = document.body;
    if (!body) return;
    body.style.transition = animated ? 'transform 0.32s cubic-bezier(0.22, 0.8, 0.3, 1)' : 'none';
    body.style.transform = 'translate3d(' + (-offset) + 'px, 0, 0)';
  }

  function show(animated) {
    R.page = Math.max(0, Math.min(R.pages - 1, R.page));
    place(R.page * pageWidth(), animated);
    fraction = R.page / R.pages;
    post({ type: 'position', page: R.page, pages: R.pages });
  }

  function pageOfAnchor(id) {
    var el = document.getElementById(id) || document.getElementsByName(id)[0];
    if (!el) return 0;
    place(0, false);
    return Math.max(0, Math.floor((el.getBoundingClientRect().left + 1) / pageWidth()));
  }

  function layout() {
    var body = document.body;
    if (!ready || !body) return;
    R.pages = Math.max(1, Math.ceil((body.scrollWidth - 2) / pageWidth()));
    if (start && start.end) R.page = R.pages - 1;
    else if (start && start.anchor) R.page = pageOfAnchor(start.anchor);
    else if (start) R.page = Math.round((start.fraction || 0) * R.pages);
    else R.page = Math.round(fraction * R.pages);
    show(false);
  }

  var layoutQueued = false;
  function queueLayout() {
    if (layoutQueued) return;
    layoutQueued = true;
    requestAnimationFrame(function () { layoutQueued = false; layout(); });
  }

  R.apply = function (s) {
    if (!s) return;
    if (s.theme) root.setAttribute('data-rn-theme', s.theme);
    if (s.fontSize) root.style.setProperty('--rn-font-size', s.fontSize + 'px');
    if (s.lineHeight) root.style.setProperty('--rn-line-height', String(s.lineHeight));
    if (s.margin != null) root.style.setProperty('--rn-margin', s.margin + 'px');
    layout();
  };
  R.configure = function (settings, where) {
    start = where || null;
    root.classList.add('rn-js');
    R.apply(settings);
  };
  R.next = function () {
    start = null;
    if (R.page < R.pages - 1) { R.page++; show(true); }
    else { show(true); post({ type: 'edge', direction: 'next' }); }
  };
  R.prev = function () {
    start = null;
    if (R.page > 0) { R.page--; show(true); }
    else { show(true); post({ type: 'edge', direction: 'prev' }); }
  };
  R.goToFraction = function (f) { start = null; R.page = Math.round(f * R.pages); show(false); };
  R.goToAnchor = function (id) { start = null; R.page = pageOfAnchor(id); show(false); };
  window.Rattonero = R;

  // ---- layout lifecycle -------------------------------------------------

  function becomeReady() {
    if (ready) return;
    ready = true;
    layout();
    root.classList.add('rn-ready');
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', becomeReady);
  else becomeReady();
  window.addEventListener('load', function () {
    becomeReady();
    layout();
    start = null;   // from here on, keep the reader's place through relayouts
  });
  window.addEventListener('resize', queueLayout);
  // Images and fonts arriving late change the page count.
  document.addEventListener('load', function (e) {
    if (e.target && e.target.tagName === 'IMG') queueLayout();
  }, true);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(queueLayout);

  // ---- input ------------------------------------------------------------

  function hasSelection() {
    var s = window.getSelection();
    return !!s && !s.isCollapsed && String(s).trim().length > 0;
  }

  var hadSelection = false;
  var suppressClickUntil = 0;

  function activate(target, clientX) {
    var link = target.closest ? target.closest('a[href]') : null;
    if (link) {
      post({ type: 'link', href: link.href && link.href.baseVal !== undefined ? link.href.baseVal : link.href });
      return;
    }
    var image = target.closest ? target.closest('img, image') : null;
    if (image) {
      var raw = image.currentSrc || image.src || image.getAttribute('href') || image.getAttribute('xlink:href');
      if (raw) {
        try { post({ type: 'image', src: new URL(raw, document.baseURI).href }); } catch (e) { /* bad URL */ }
        return;
      }
    }
    if (hadSelection || hasSelection()) return;   // this tap only dismissed a selection
    var x = clientX / pageWidth();
    if (x < 0.3) R.prev();
    else if (x > 0.7) R.next();
    else post({ type: 'tap' });
  }

  document.addEventListener('mousedown', function () { hadSelection = hasSelection(); }, true);
  document.addEventListener('click', function (e) {
    e.preventDefault();   // never navigate by ourselves; the host decides
    if (Date.now() < suppressClickUntil) return;
    activate(e.target, e.clientX);
  }, true);

  // Touch: the page follows the finger, then settles on a page.
  var touch = null;
  document.addEventListener('touchstart', function (e) {
    hadSelection = hasSelection();
    if (e.touches.length !== 1) { touch = null; return; }
    var t = e.touches[0];
    touch = { x: t.clientX, y: t.clientY, time: Date.now(), dx: 0, moved: false, dragging: false, target: e.target };
  }, { passive: true });
  document.addEventListener('touchmove', function (e) {
    if (!touch || e.touches.length !== 1) return;
    var t = e.touches[0];
    var dx = t.clientX - touch.x, dy = t.clientY - touch.y;
    if (Math.abs(dx) > 8 || Math.abs(dy) > 8) touch.moved = true;
    if (!touch.dragging) {
      if (hasSelection()) { touch = null; return; }   // adjusting a selection
      if (Math.abs(dx) < 10 || Math.abs(dx) < Math.abs(dy) * 1.5) return;
      touch.dragging = true;
    }
    touch.dx = dx;
    var atEdge = (R.page === 0 && dx > 0) || (R.page === R.pages - 1 && dx < 0);
    place(R.page * pageWidth() - (atEdge ? dx / 3 : dx), false);
  }, { passive: true });
  document.addEventListener('touchend', function () {
    var t = touch;
    touch = null;
    if (!t) return;
    suppressClickUntil = Date.now() + 400;
    if (t.dragging) {
      var quick = Date.now() - t.time < 300;
      var threshold = quick ? 30 : pageWidth() * 0.25;
      if (t.dx <= -threshold) R.next();
      else if (t.dx >= threshold) R.prev();
      else show(true);
    } else if (!t.moved && Date.now() - t.time < 400) {
      activate(t.target, t.x);
    }
  });
  document.addEventListener('touchcancel', function () { if (touch && touch.dragging) show(true); touch = null; });

  // Trackpad swipes and the mouse wheel: one gesture, one page.
  var wheelSum = 0, wheelLocked = false, wheelTimer = null;
  document.addEventListener('wheel', function (e) {
    e.preventDefault();
    if (e.ctrlKey) return;
    var delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    clearTimeout(wheelTimer);
    wheelTimer = setTimeout(function () { wheelSum = 0; wheelLocked = false; }, 160);
    if (wheelLocked) return;
    wheelSum += delta;
    if (Math.abs(wheelSum) > 40) {
      wheelLocked = true;
      if (wheelSum > 0) R.next(); else R.prev();
      wheelSum = 0;
    }
  }, { passive: false });

  document.addEventListener('keydown', function (e) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    var forward = e.key === 'ArrowRight' || e.key === 'PageDown' || e.key === 'ArrowDown' || (e.key === ' ' && !e.shiftKey);
    var back = e.key === 'ArrowLeft' || e.key === 'PageUp' || e.key === 'ArrowUp' || (e.key === ' ' && e.shiftKey);
    if (!forward && !back) return;
    e.preventDefault();
    if (forward) R.next(); else R.prev();
  });

  var selectionTimer = null;
  document.addEventListener('selectionchange', function () {
    clearTimeout(selectionTimer);
    selectionTimer = setTimeout(function () {
      var s = window.getSelection();
      var text = s ? String(s).replace(/\s+/g, ' ').trim() : '';
      var rect = null;
      if (text && s.rangeCount) {
        var r = s.getRangeAt(0).getBoundingClientRect();
        rect = { x: r.left, y: r.top, width: r.width, height: r.height };
      }
      post({ type: 'selection', text: text.slice(0, 200), rect: rect });
    }, 250);
  });
})();
