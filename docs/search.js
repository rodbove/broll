// Page search — activated by "/" or Cmd/Ctrl+K
(function () {
  var overlay = document.getElementById('search-overlay');
  var input = document.getElementById('search-input');
  var resultsContainer = document.getElementById('search-results');
  var activeIndex = -1;

  // Build search index from page content
  var searchIndex = [];

  function buildIndex() {
    searchIndex = [];

    // Index sections by their titles and text content
    var sections = document.querySelectorAll('.section, .hero');
    sections.forEach(function (section) {
      var id = section.id || section.querySelector('[id]')?.id;
      if (!id) return;

      var title = section.querySelector('.section-title, .hero-title');
      var titleText = title ? title.textContent.trim() : '';
      var label = section.querySelector('.section-label');
      var labelText = label ? label.textContent.trim() : '';

      // Get all text content for searching
      var textContent = section.textContent.replace(/\s+/g, ' ').trim();

      if (titleText) {
        searchIndex.push({
          section: labelText || 'Page',
          title: titleText,
          text: textContent.substring(0, 300),
          href: '#' + id
        });
      }
    });

    // Index command references
    var cmdRefs = document.querySelectorAll('.cmd-ref');
    cmdRefs.forEach(function (ref) {
      var id = ref.id;
      if (!id) return;

      var name = ref.querySelector('.cmd-ref-name');
      var desc = ref.querySelector('.cmd-ref-desc');
      var badge = ref.querySelector('.cmd-ref-badge');

      searchIndex.push({
        section: badge ? badge.textContent.trim() : 'Command',
        title: name ? name.textContent.trim() : '',
        text: desc ? desc.textContent.trim() : '',
        href: '#' + id
      });
    });

    // Index keybinding groups
    var kbGroups = document.querySelectorAll('.keybindings-group');
    kbGroups.forEach(function (group) {
      var h3 = group.querySelector('h3');
      var text = group.textContent.replace(/\s+/g, ' ').trim();
      searchIndex.push({
        section: 'Keybindings',
        title: h3 ? h3.textContent.trim() : 'Keybindings',
        text: text.substring(0, 200),
        href: '#keybindings'
      });
    });

    // Index feature cards
    var featureCards = document.querySelectorAll('.feature-card');
    featureCards.forEach(function (card) {
      var h3 = card.querySelector('h3');
      var p = card.querySelector('p');
      if (h3) {
        searchIndex.push({
          section: 'Features',
          title: h3.textContent.trim(),
          text: p ? p.textContent.trim() : '',
          href: '#features'
        });
      }
    });

    // Index filter table rows
    var filterRows = document.querySelectorAll('.filter-table tbody tr');
    filterRows.forEach(function (row) {
      var cells = row.querySelectorAll('td');
      if (cells.length >= 2) {
        searchIndex.push({
          section: 'Security',
          title: cells[0].textContent.trim(),
          text: cells[1].textContent.trim(),
          href: '#filtering'
        });
      }
    });
  }

  function search(query) {
    if (!query.trim()) {
      resultsContainer.innerHTML = '';
      resultsContainer.removeAttribute('data-empty');
      activeIndex = -1;
      return;
    }

    var terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    var scored = [];

    searchIndex.forEach(function (item) {
      var haystack = (item.title + ' ' + item.text + ' ' + item.section).toLowerCase();
      var allMatch = terms.every(function (term) { return haystack.indexOf(term) !== -1; });
      if (!allMatch) return;

      // Score: title matches are worth more
      var titleLower = item.title.toLowerCase();
      var score = 0;
      terms.forEach(function (term) {
        if (titleLower.indexOf(term) !== -1) score += 10;
        if (titleLower === query.toLowerCase()) score += 20;
      });
      score += 1; // base score for matching

      scored.push({ item: item, score: score });
    });

    scored.sort(function (a, b) { return b.score - a.score; });
    var results = scored.slice(0, 12);

    if (results.length === 0) {
      resultsContainer.innerHTML = '';
      resultsContainer.setAttribute('data-empty', 'true');
      activeIndex = -1;
      return;
    }

    resultsContainer.removeAttribute('data-empty');
    resultsContainer.innerHTML = results.map(function (r, i) {
      var preview = r.item.text;
      // Highlight first matching term in preview
      var idx = preview.toLowerCase().indexOf(terms[0]);
      if (idx > 40) {
        preview = '...' + preview.substring(idx - 20);
      }
      if (preview.length > 120) {
        preview = preview.substring(0, 120) + '...';
      }

      return '<a class="search-result-item' + (i === 0 ? ' active' : '') + '" href="' + r.item.href + '" data-index="' + i + '">' +
        '<div class="search-result-section">' + escapeHtml(r.item.section) + '</div>' +
        '<div class="search-result-title">' + escapeHtml(r.item.title) + '</div>' +
        '<div class="search-result-preview">' + escapeHtml(preview) + '</div>' +
        '</a>';
    }).join('');

    activeIndex = 0;
  }

  function escapeHtml(str) {
    var div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }

  function setActive(index) {
    var items = resultsContainer.querySelectorAll('.search-result-item');
    if (items.length === 0) return;

    items.forEach(function (el) { el.classList.remove('active'); });
    activeIndex = Math.max(0, Math.min(index, items.length - 1));
    items[activeIndex].classList.add('active');
    items[activeIndex].scrollIntoView({ block: 'nearest' });
  }

  function openSearch() {
    buildIndex();
    overlay.classList.add('open');
    input.value = '';
    resultsContainer.innerHTML = '';
    resultsContainer.removeAttribute('data-empty');
    activeIndex = -1;
    // Delay focus slightly to avoid the triggering "/" being typed
    setTimeout(function () { input.focus(); }, 10);
  }

  function closeSearch() {
    overlay.classList.remove('open');
    input.value = '';
    resultsContainer.innerHTML = '';
    activeIndex = -1;
  }

  function navigateToResult() {
    var items = resultsContainer.querySelectorAll('.search-result-item');
    if (activeIndex >= 0 && activeIndex < items.length) {
      var href = items[activeIndex].getAttribute('href');
      closeSearch();
      window.location.hash = href;
    }
  }

  // Keyboard shortcuts
  document.addEventListener('keydown', function (e) {
    var isOpen = overlay.classList.contains('open');

    // Open: "/" when not in input, or Cmd/Ctrl+K
    if (!isOpen) {
      if (e.key === '/' && !isInputFocused()) {
        e.preventDefault();
        openSearch();
        return;
      }
      if (e.key === 'k' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        openSearch();
        return;
      }
      return;
    }

    // Close on Escape
    if (e.key === 'Escape') {
      e.preventDefault();
      closeSearch();
      return;
    }

    // Navigate results
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive(activeIndex + 1);
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive(activeIndex - 1);
      return;
    }

    // Select result
    if (e.key === 'Enter') {
      e.preventDefault();
      navigateToResult();
      return;
    }
  });

  // Search on input
  input.addEventListener('input', function () {
    search(input.value);
  });

  // Close on overlay click (outside dialog)
  overlay.addEventListener('click', function (e) {
    if (e.target === overlay) {
      closeSearch();
    }
  });

  // Click on result
  resultsContainer.addEventListener('click', function (e) {
    var item = e.target.closest('.search-result-item');
    if (item) {
      e.preventDefault();
      var href = item.getAttribute('href');
      closeSearch();
      window.location.hash = href;
    }
  });

  // Hover to set active
  resultsContainer.addEventListener('mousemove', function (e) {
    var item = e.target.closest('.search-result-item');
    if (item) {
      var idx = parseInt(item.getAttribute('data-index'), 10);
      if (idx !== activeIndex) setActive(idx);
    }
  });

  function isInputFocused() {
    var el = document.activeElement;
    return el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
  }
})();
