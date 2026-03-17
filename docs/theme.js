// Theme toggle: Original (light) / Sunset (dark)
// Respects system preference, with manual override stored in localStorage

(function () {
  const STORAGE_KEY = 'broll-theme';
  const toggle = document.getElementById('theme-toggle');

  function getSystemTheme() {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }

  function apply(theme) {
    if (theme === 'dark') {
      document.documentElement.setAttribute('data-theme', 'dark');
      toggle.textContent = '\u2600\uFE0F';
      toggle.title = 'Switch to light theme';
    } else {
      document.documentElement.removeAttribute('data-theme');
      toggle.textContent = '\uD83C\uDF19';
      toggle.title = 'Switch to dark theme';
    }
  }

  // Initialize
  var stored = localStorage.getItem(STORAGE_KEY);
  var initial = stored || getSystemTheme();
  apply(initial);

  // Toggle on click
  toggle.addEventListener('click', function () {
    var current = document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';
    var next = current === 'dark' ? 'light' : 'dark';
    localStorage.setItem(STORAGE_KEY, next);
    apply(next);
  });

  // Listen for system preference changes (only if no manual override)
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', function (e) {
    if (!localStorage.getItem(STORAGE_KEY)) {
      apply(e.matches ? 'dark' : 'light');
    }
  });

  // Mobile nav toggle
  var navToggle = document.getElementById('nav-toggle');
  var navLinks = document.getElementById('nav-links');
  if (navToggle && navLinks) {
    navToggle.addEventListener('click', function () {
      navLinks.classList.toggle('open');
    });
    // Close mobile nav when clicking a link
    navLinks.querySelectorAll('a').forEach(function (a) {
      a.addEventListener('click', function () {
        navLinks.classList.remove('open');
      });
    });
  }
})();
