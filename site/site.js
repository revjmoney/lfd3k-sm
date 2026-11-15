// LFD3K-SM website: the insert cards and the click-to-enlarge lightbox.
(function () {
  "use strict";
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // ---------------------------------------------------------------- insert cards
  var deck = document.getElementById("deck");
  var cards = deck ? Array.prototype.slice.call(deck.querySelectorAll(".card")) : [];
  var current = null;   // the card showing
  var opener = null;    // the link that opened the deck, for focus on close
  var busy = false;

  function byId(id) {
    for (var i = 0; i < cards.length; i++) if (cards[i].id === id) return cards[i];
    return null;
  }

  function afterSlide(fn) {
    // run fn once the card's slide ends (or at once with reduced motion)
    if (reduce) { fn(); return; }
    var shell = deck.querySelector(".card-shell");
    var done = false;
    function go() { if (done) return; done = true; shell.removeEventListener("transitionend", onEnd); fn(); }
    function onEnd(e) { if (e.target === shell && e.propertyName === "transform") go(); }
    shell.addEventListener("transitionend", onEnd);
    setTimeout(go, 520);
  }

  function pauseMedia(card) {
    if (!card) return;
    Array.prototype.forEach.call(card.querySelectorAll("video"), function (v) { v.pause(); });
  }

  function show(card, focusId) {
    cards.forEach(function (c) { c.hidden = c !== card; });
    current = card;
    deck.querySelector(".card-kicker").textContent = card.getAttribute("data-title") || "LFD3K-SM";
    var i = cards.indexOf(card);
    var prev = cards[(i - 1 + cards.length) % cards.length];
    var next = cards[(i + 1) % cards.length];
    deck.querySelector(".card-prev").textContent = "Back: " + prev.getAttribute("data-title");
    deck.querySelector(".card-next").textContent = "Next: " + next.getAttribute("data-title");
    var scroller = deck.querySelector(".card-scroll");
    scroller.scrollTop = 0;
    if (focusId) {
      var t = card.querySelector("#" + CSS.escape(focusId));
      if (t) {
        t.scrollIntoView({ block: "center" });
        if (t.classList.contains("reason")) {
          t.classList.add("hit");
          setTimeout(function () { t.classList.remove("hit"); }, 1600);
        }
      }
    }
  }

  function slideIn() {
    // two frames so the starting position is painted before the slide
    requestAnimationFrame(function () { requestAnimationFrame(function () {
      deck.classList.add("in");
      deck.querySelector(".card-scroll").focus({ preventScroll: true });
    }); });
  }

  function openCard(id, focusId, link) {
    var card = byId(id);
    if (!card || busy) return false;
    if (!deck.open) {
      opener = link || document.activeElement;
      show(card, null);
      document.documentElement.style.overflow = "hidden";
      deck.showModal();
      slideIn();
      if (focusId) afterSlide(function () { show(card, focusId); });
      return true;
    }
    if (card === current) { if (focusId) show(card, focusId); return true; }
    // swap: the open card slides out, the new one slides in
    busy = true;
    pauseMedia(current);
    deck.classList.add("leaving");
    deck.classList.remove("in");
    afterSlide(function () {
      deck.classList.remove("leaving");
      show(card, null);
      slideIn();
      afterSlide(function () { busy = false; if (focusId) show(card, focusId); });
    });
    return true;
  }

  function closeCard() {
    if (!deck.open || busy) return;
    busy = true;
    pauseMedia(current);
    deck.classList.add("leaving");
    deck.classList.remove("in");
    afterSlide(function () {
      deck.classList.remove("leaving");
      deck.close();
      busy = false;
    });
  }

  if (deck) {
    deck.addEventListener("close", function () {
      deck.classList.remove("in", "leaving");
      document.documentElement.style.overflow = "";
      if (location.hash) history.replaceState(null, "", location.pathname + location.search);
      if (opener && opener.focus) opener.focus();
      opener = null;
    });
    // Esc slides the card out instead of snapping it shut
    deck.addEventListener("cancel", function (e) { e.preventDefault(); closeCard(); });
    // a click outside the card (on the dimmed page) closes it
    deck.addEventListener("click", function (e) { if (e.target === deck) closeCard(); });
    deck.querySelector(".card-close").addEventListener("click", closeCard);
    deck.querySelector(".card-prev").addEventListener("click", function () {
      var i = cards.indexOf(current);
      go(cards[(i - 1 + cards.length) % cards.length].id, null, null);
    });
    deck.querySelector(".card-next").addEventListener("click", function () {
      var i = cards.indexOf(current);
      go(cards[(i + 1) % cards.length].id, null, null);
    });

    // every link with data-card opens its card; the address bar follows, so back works
    document.addEventListener("click", function (e) {
      var a = e.target.closest && e.target.closest("a[data-card]");
      if (!a || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
      e.preventDefault();
      go(a.getAttribute("data-card"), a.getAttribute("data-focus"), a);
    });

    window.addEventListener("popstate", function () { fromHash(); });

    // a link straight to a card (for example the README's #call) opens it
    fromHash();
  }

  function go(id, focusId, link) {
    if (busy) return;
    if (location.hash !== "#" + id) history.pushState(null, "", "#" + id);
    openCard(id, focusId, link);
  }

  function fromHash() {
    var id = decodeURIComponent(location.hash.replace(/^#/, ""));
    if (id && byId(id)) openCard(id, null, null);
    else if (id === "tiers") openCard("why", "tiers", null);
    else if (deck.open) closeCard();
  }

  // ---------------------------------------------------------------- lightbox
  var dlg = document.getElementById("lightbox");
  if (!dlg) return;
  var img = dlg.querySelector(".lb-img");
  var cap = dlg.querySelector(".lb-cap");
  var group = [];
  var index = 0;
  var lbOpener = null;

  function lbShow(i) {
    index = (i + group.length) % group.length;
    var a = group[index];
    var thumb = a.querySelector("img");
    // keep pixel art crisp when it is blown up; let big screenshots scale smoothly
    var pixel = !!(thumb && thumb.classList.contains("pixel"));
    img.classList.toggle("pixel", pixel);
    img.style.width = "";
    img.onload = function () {
      if (!pixel) return;
      // blow pixel art up by the largest whole multiple that fits the screen
      var k = Math.floor(Math.min(window.innerWidth * 0.94 / img.naturalWidth, window.innerHeight * 0.78 / img.naturalHeight));
      if (k >= 1) img.style.width = (img.naturalWidth * k) + "px";
    };
    img.alt = thumb ? thumb.alt : "";
    img.src = a.getAttribute("href");
    var fc = a.closest("figure") && a.closest("figure").querySelector("figcaption");
    cap.textContent = (fc ? fc.textContent : img.alt) + "  (" + (index + 1) + "/" + group.length + ")";
  }

  function lbOpen(a) {
    var gal = a.closest("[data-gallery]");
    group = Array.prototype.slice.call(gal ? gal.querySelectorAll("a.shot") : [a]);
    lbOpener = a;
    lbShow(group.indexOf(a));
    if (typeof dlg.showModal === "function") dlg.showModal(); else dlg.setAttribute("open", "");
    dlg.querySelector(".lb-close").focus();
  }

  function lbClose() {
    if (typeof dlg.close === "function") dlg.close(); else dlg.removeAttribute("open");
  }

  dlg.addEventListener("close", function () {
    img.removeAttribute("src");
    if (lbOpener) lbOpener.focus();
  });

  document.addEventListener("click", function (e) {
    var a = e.target.closest && e.target.closest("a.shot");
    if (!a || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    lbOpen(a);
  });

  dlg.querySelector(".lb-prev").addEventListener("click", function () { lbShow(index - 1); });
  dlg.querySelector(".lb-next").addEventListener("click", function () { lbShow(index + 1); });
  dlg.querySelector(".lb-close").addEventListener("click", lbClose);
  dlg.addEventListener("click", function (e) { if (e.target === dlg) lbClose(); });

  document.addEventListener("keydown", function (e) {
    if (!dlg.open) return;
    if (e.key === "ArrowRight") { e.preventDefault(); lbShow(index + 1); }
    else if (e.key === "ArrowLeft") { e.preventDefault(); lbShow(index - 1); }
    else if (e.key === "Escape") { e.preventDefault(); lbClose(); }
  });
})();
