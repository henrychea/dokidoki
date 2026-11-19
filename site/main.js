(() => {
  const root = document.documentElement;
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const prefersLight = matchMedia("(prefers-color-scheme: light)");
  const status = document.getElementById("status");
  const isLight = () => root.dataset.theme ? root.dataset.theme === "light" : prefersLight.matches;

  const toggle = document.querySelector("[data-theme-toggle]");
  const syncToggle = () => toggle.setAttribute("aria-label", isLight() ? "Switch to dark theme" : "Switch to light theme");
  toggle.addEventListener("click", () => {
    const next = isLight() ? "dark" : "light";
    root.dataset.theme = next;
    try { localStorage.setItem("doki-theme", next); } catch (e) {}
    syncToggle();
    showShot(next === "light" ? "washi" : "sumi");
  });
  prefersLight.addEventListener("change", () => { syncToggle(); if (!root.dataset.theme) showShot(isLight() ? "washi" : "sumi"); });
  syncToggle();

  const shot = document.getElementById("shot");
  const shotButtons = document.querySelectorAll("[data-shot]");
  const alts = {
    sumi: "dokidoki in the Sumi dark theme: repository tree on the left, AI Commit Studio with a proposed doki: subject and a diff in the center, journal and logs on the right",
    washi: "dokidoki in the Washi light theme: repository tree on the left, AI Commit Studio with a proposed doki: subject and a diff in the center, journal and logs on the right"
  };
  function showShot(name) {
    shotButtons.forEach(b => b.setAttribute("aria-pressed", String(b.dataset.shot === name)));
    if (shot.getAttribute("src").includes(name)) return;
    const swap = () => { shot.src = `assets/${name}.png`; shot.alt = alts[name]; shot.classList.remove("fade"); };
    if (reduced) return swap();
    shot.classList.add("fade");
    setTimeout(swap, 200);
  }
  shotButtons.forEach(b => b.addEventListener("click", () => showShot(b.dataset.shot)));
  showShot(isLight() ? "washi" : "sumi");

  document.querySelectorAll(".copy").forEach(btn => {
    btn.addEventListener("click", async () => {
      const text = btn.parentElement.querySelector("code").textContent;
      try {
        await navigator.clipboard.writeText(text);
        btn.textContent = "Copied";
        btn.classList.add("done");
        status.textContent = "Copied to clipboard";
      } catch (e) {
        btn.textContent = "Copy failed";
        status.textContent = "Copy failed; select the text instead";
      }
      setTimeout(() => { btn.textContent = "Copy"; btn.classList.remove("done"); }, 1600);
    });
  });

  if (reduced) return;

  const petals = document.querySelector(".petals");
  for (let i = 0; i < 18; i++) {
    const p = document.createElement("span");
    p.className = i % 4 === 0 ? "petal ink" : "petal";
    p.style.left = `${Math.random() * 100}%`;
    p.style.setProperty("--dx", `${Math.round(Math.random() * 160 - 40)}px`);
    p.style.animationDuration = `${8 + Math.random() * 8}s`;
    p.style.animationDelay = `${-Math.random() * 16}s`;
    petals.appendChild(p);
  }
  new IntersectionObserver(([e]) => petals.classList.toggle("paused", !e.isIntersecting)).observe(document.querySelector(".hero"));

  const pre = document.querySelector("[data-typed]");
  const code = pre.querySelector("code");
  const finalHTML = code.innerHTML;
  const lines = finalHTML.split("\n");
  let started = false;

  // Typing replays the static markup line by line, so screen readers and no-JS users still get the full text.
  async function play() {
    const wait = ms => new Promise(r => setTimeout(r, ms));
    pre.setAttribute("aria-busy", "true");
    for (;;) {
      code.innerHTML = "";
      for (const line of lines) {
        const isCmd = line.startsWith('<span class="p">$</span>');
        const row = document.createElement("span");
        code.appendChild(row);
        if (isCmd) {
          const cmd = line.replace('<span class="p">$</span> ', "");
          row.innerHTML = '<span class="p">$</span> ';
          const t = document.createTextNode("");
          const caret = document.createElement("span");
          caret.className = "caret";
          row.append(t, caret);
          for (const ch of cmd) { t.data += ch; await wait(45 + Math.random() * 50); }
          await wait(350);
          caret.remove();
        } else {
          row.innerHTML = line;
          await wait(line.includes("agent") ? 700 : 160);
        }
        code.appendChild(document.createTextNode("\n"));
      }
      pre.removeAttribute("aria-busy");
      const heart = code.querySelector(".heart");
      if (heart) heart.classList.add("beat-now");
      await wait(5000);
    }
  }
  new IntersectionObserver(([e], obs) => {
    if (e.isIntersecting && !started) { started = true; obs.disconnect(); play(); }
  }).observe(pre);
})();
