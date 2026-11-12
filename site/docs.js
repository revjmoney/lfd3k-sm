// LFD3K-SM docs page: renders the repo's markdown at view time with marked, sanitized by DOMPurify.
// Deep links: docs.html#features, or docs.html#features/at-commands for a heading.
(function () {
  "use strict";

  var REPO = "https://github.com/revjmoney/lfd3k-sm/blob/main/";
  var DOCS = [
    { id: "readme", title: "Overview (README)", path: "README.md" },
    { id: "features", title: "Features, settings, AT commands", path: "docs/FEATURES.md" },
    { id: "design", title: "Design", path: "docs/2026-11-09-lfd3k-sm-design.md" },
    { id: "multimode", title: "Multimode guide", path: "docs/multimode.md" },
    { id: "multimode-readme", title: "Multimode engineering", path: "multimode/README.md" },
    { id: "simnet", title: "SimNet", path: "docs/simnet.md" },
    { id: "phase2", title: "Bench sheet: phase 2", path: "docs/phase2-bench.md" },
    { id: "phase3", title: "Bench sheet: phase 3 (Wi-Fi)", path: "docs/phase3-bench.md" },
    { id: "phase4", title: "Bench sheet: phase 4 (menu)", path: "docs/phase4-bench.md" },
    { id: "notice", title: "Notice and attribution", path: "NOTICE.md" },
    { id: "third-party", title: "Third-party notices", path: "THIRD_PARTY.md" }
  ];
  var byPath = {};
  DOCS.forEach(function (d) { byPath[d.path.toLowerCase()] = d; });

  var list = document.getElementById("doc-list");
  var article = document.getElementById("doc");
  var meta = document.getElementById("doc-meta");
  var cache = {};

  DOCS.forEach(function (d) {
    var li = document.createElement("li");
    var a = document.createElement("a");
    a.href = "#" + d.id;
    a.dataset.id = d.id;
    a.textContent = d.title;
    var src = document.createElement("span");
    src.className = "src";
    src.textContent = d.path;
    a.appendChild(src);
    li.appendChild(a);
    list.appendChild(li);
  });

  function slug(text) {
    return text.toLowerCase().trim()
      .replace(/[^\w\s-]/g, "")
      .replace(/\s+/g, "-")
      .replace(/-+/g, "-")
      .replace(/^-|-$/g, "");
  }

  // Resolve a relative URL against the markdown file's own folder; returns a site-relative path.
  function resolve(docPath, rel) {
    var base = docPath.split("/").slice(0, -1);
    var parts = rel.split("/");
    parts.forEach(function (p) {
      if (p === "" || p === ".") return;
      if (p === "..") base.pop(); else base.push(p);
    });
    return base.join("/");
  }

  function isExternal(u) { return /^([a-z][a-z0-9+.-]*:|\/\/)/i.test(u); }

  function rewrite(root, doc) {
    root.querySelectorAll("img[src]").forEach(function (img) {
      var src = img.getAttribute("src");
      if (isExternal(src) || src.charAt(0) === "/") return;
      img.setAttribute("src", resolve(doc.path, src));
      img.setAttribute("loading", "lazy");
    });
    root.querySelectorAll("a[href]").forEach(function (a) {
      var href = a.getAttribute("href");
      if (isExternal(href)) {
        if (/^https?:/i.test(href)) a.setAttribute("rel", "noopener");
        return;
      }
      if (href.charAt(0) === "#") {
        a.setAttribute("href", "#" + doc.id + "/" + href.slice(1));
        return;
      }
      var hashAt = href.indexOf("#");
      var pathPart = hashAt >= 0 ? href.slice(0, hashAt) : href;
      var frag = hashAt >= 0 ? href.slice(hashAt + 1) : "";
      var target = resolve(doc.path, pathPart);
      var known = byPath[target.toLowerCase()];
      if (known) {
        a.setAttribute("href", "#" + known.id + (frag ? "/" + frag : ""));
      } else {
        // anything not published on the site (LICENSE, source, txt) goes to GitHub
        a.setAttribute("href", REPO + target.replace(/\/$/, "") + (frag ? "#" + frag : ""));
      }
    });
    var seen = {};
    root.querySelectorAll("h1, h2, h3, h4").forEach(function (h) {
      var s = slug(h.textContent) || "section";
      if (seen[s]) { seen[s] += 1; s = s + "-" + seen[s]; } else { seen[s] = 1; }
      h.id = doc.id + "/" + s;
    });
    root.querySelectorAll("table").forEach(function (t) {
      var wrap = document.createElement("div");
      wrap.className = "table-scroll";
      t.parentNode.insertBefore(wrap, t);
      wrap.appendChild(t);
    });
  }

  function parseHash() {
    var h = decodeURIComponent(location.hash.replace(/^#/, ""));
    var slash = h.indexOf("/");
    var id = slash >= 0 ? h.slice(0, slash) : h;
    var doc = DOCS.filter(function (d) { return d.id === id; })[0] || DOCS[0];
    return { doc: doc, anchor: slash >= 0 && doc.id === id ? h : "" };
  }

  function fetchDoc(doc) {
    if (cache[doc.id]) return Promise.resolve(cache[doc.id]);
    return fetch(doc.path).then(function (r) {
      if (!r.ok) throw new Error(r.status + " " + r.statusText);
      return r.text();
    }).then(function (t) { cache[doc.id] = t; return t; });
  }

  var current = null;
  function route() {
    var r = parseHash();
    list.querySelectorAll("a").forEach(function (a) {
      if (a.dataset.id === r.doc.id) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    });
    if (current === r.doc.id) { scrollTo(r.anchor); return; }
    current = r.doc.id;
    document.title = r.doc.title + " | LFD3K-SM Documentation";
    meta.innerHTML = "";
    meta.appendChild(document.createTextNode(r.doc.path + " · "));
    var src = document.createElement("a");
    src.href = REPO + r.doc.path;
    src.textContent = "view on GitHub";
    meta.appendChild(src);
    article.innerHTML = '<p class="doc-status">Loading ' + r.doc.path + "...</p>";
    fetchDoc(r.doc).then(function (md) {
      if (current !== r.doc.id) return;
      if (!window.marked || !window.DOMPurify) throw new Error("the markdown renderer did not load");
      var html = window.marked.parse(md, { gfm: true });
      var clean = window.DOMPurify.sanitize(html);
      var tmp = document.createElement("div");
      tmp.innerHTML = clean;
      rewrite(tmp, r.doc);
      article.innerHTML = "";
      while (tmp.firstChild) article.appendChild(tmp.firstChild);
      if (r.anchor) scrollTo(r.anchor); else window.scrollTo(0, 0);
    }).catch(function (err) {
      if (current !== r.doc.id) return;
      article.innerHTML = "";
      var p = document.createElement("p");
      p.className = "doc-status";
      p.appendChild(document.createTextNode("Could not load " + r.doc.path + " (" + err.message + "). Read it on "));
      var a = document.createElement("a");
      a.href = REPO + r.doc.path;
      a.textContent = "GitHub";
      p.appendChild(a);
      p.appendChild(document.createTextNode("."));
      article.appendChild(p);
    });
  }

  function scrollTo(anchor) {
    if (!anchor) return;
    var el = document.getElementById(anchor);
    if (el) el.scrollIntoView();
  }

  window.addEventListener("hashchange", route);
  route();
})();
