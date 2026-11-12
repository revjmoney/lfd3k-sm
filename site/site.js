// LFD3K-SM website: click-to-enlarge lightbox. Esc closes, arrow keys move within a gallery.
(function () {
  "use strict";
  var dlg = document.getElementById("lightbox");
  if (!dlg) return;
  var img = dlg.querySelector(".lb-img");
  var cap = dlg.querySelector(".lb-cap");
  var group = [];
  var index = 0;
  var opener = null;

  function show(i) {
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

  function open(a) {
    var gal = a.closest("[data-gallery]");
    group = Array.prototype.slice.call(gal ? gal.querySelectorAll("a.shot") : [a]);
    opener = a;
    show(group.indexOf(a));
    if (typeof dlg.showModal === "function") dlg.showModal(); else dlg.setAttribute("open", "");
    dlg.querySelector(".lb-close").focus();
  }

  function close() {
    if (typeof dlg.close === "function") dlg.close(); else dlg.removeAttribute("open");
  }

  dlg.addEventListener("close", function () {
    img.removeAttribute("src");
    if (opener) opener.focus();
  });

  document.addEventListener("click", function (e) {
    var a = e.target.closest && e.target.closest("a.shot");
    if (!a || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    open(a);
  });

  dlg.querySelector(".lb-prev").addEventListener("click", function () { show(index - 1); });
  dlg.querySelector(".lb-next").addEventListener("click", function () { show(index + 1); });
  dlg.querySelector(".lb-close").addEventListener("click", close);
  // click on the dark backdrop closes
  dlg.addEventListener("click", function (e) { if (e.target === dlg) close(); });

  document.addEventListener("keydown", function (e) {
    if (!dlg.open) return;
    if (e.key === "ArrowRight") { e.preventDefault(); show(index + 1); }
    else if (e.key === "ArrowLeft") { e.preventDefault(); show(index - 1); }
    else if (e.key === "Escape") { e.preventDefault(); close(); }
  });
})();
