// Runs in a CDP isolated world. Page content cannot access the action map.
// This is Kindred's browser protocol, not an OpenAI Decisions request schema.
(() => {
  const MAX_CHOICES = 256, MAX_ELEMENTS = 160;
  let snapshot = null;
  const visible = element => {
    const rect = element.getBoundingClientRect(), style = element.ownerDocument.defaultView.getComputedStyle(element);
    return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
  };
  const editable = element => element instanceof HTMLTextAreaElement ||
    (element instanceof HTMLInputElement && ['text', 'search', 'url', 'email', 'tel', 'number'].includes(element.type));
  const forbidden = element => element.matches('input[type="password"],input[autocomplete*="password"],input[autocomplete="one-time-code"],input[type="file"]');
  function authenticationVisible(root = document) {
    for (const element of root.querySelectorAll('*')) {
      if (element.shadowRoot && authenticationVisible(element.shadowRoot)) return true;
      if (element.matches('input[type="password"],input[autocomplete*="password"],input[autocomplete="one-time-code"]') && visible(element)) return true;
    }
    return false;
  }
  // An isolated world's security origin may be null. Resolve the actual page
  // URL rather than treating the isolated world as the page's origin.
  const safeToCapture = origin => new URL(location.href).origin === origin && document.visibilityState === 'visible' && document.hasFocus() && !authenticationVisible();
  const labelText = label => {
    const clone = label.cloneNode(true);
    for (const control of clone.querySelectorAll('input,textarea,select,button')) control.remove();
    return clone.textContent;
  };
  const name = element => {
    const refs = (element.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
    return (refs.map(id => element.ownerDocument.getElementById(id)?.textContent || '').join(' ') ||
      element.getAttribute('aria-label') || Array.from(element.labels || []).map(labelText).join(' ') ||
      element.getAttribute('placeholder') || element.innerText || element.getAttribute('title') || element.getAttribute('name') ||
      element.getAttribute('id') || element.tagName.toLowerCase()).trim().replace(/\s+/g, ' ').slice(0, 300);
  };
  const checked = element => element.matches('input[type="checkbox"],input[type="radio"]') ? element.checked : element.getAttribute('aria-checked') === 'true';
  function elements() {
    const found = [];
    function visit(root) {
      for (const element of root.querySelectorAll('*')) {
        if (element.shadowRoot) visit(element.shadowRoot);
        // Frames need their own coordinate/context mapping. Explicitly fall back
        // instead of guessing that a frame's coordinates refer to this page.
        if (element.tagName === 'IFRAME' && visible(element)) throw new Error('Frame requires ordinary computer tools');
        if (!visible(element) || forbidden(element)) continue;
        if (element.matches('button,a[href],input,textarea,select,[role="button"],[role="checkbox"],[role="radio"],[role="switch"]')) {
          found.push(element);
          if (found.length > MAX_ELEMENTS) throw new Error('Page exceeds browser element limit');
        }
      }
    }
    visit(document);
    return found;
  }
  function signature(list) {
    return JSON.stringify([location.href, list.map(element => {
      const rect = element.getBoundingClientRect();
      return [element.tagName, element.type, name(element), element.disabled || element.getAttribute('aria-disabled') === 'true',
        checked(element), editable(element) || element instanceof HTMLSelectElement ? element.value : null,
        element.getAttribute('href'), Array.from(element.options || []).map(o => [o.value, o.text, o.disabled]),
        rect.x, rect.y, rect.width, rect.height];
    })]);
  }
  function observe(values, snapshotId) {
    snapshot = null;
    if (!safeToCapture(new URL(location.href).origin)) throw new Error('Authentication view needs the normal human-interaction flow');
    const list = elements(), actions = new Map(), candidates = [];
    const add = (kind, label, data = {}, value_key = undefined) => {
      if (candidates.length >= MAX_CHOICES - 4) throw new Error('Page exceeds browser choice limit');
      const id = 'a' + candidates.length;
      actions.set(id, {kind, ...data});
      candidates.push({id, kind, label, value_key});
    };
    for (const [index, element] of list.entries()) {
      if (element.disabled || element.readOnly || element.getAttribute('aria-disabled') === 'true') continue;
      const label = name(element), suffix = ` [control ${index}]`;
      if (element.matches('input[type="checkbox"],[role="checkbox"],[role="switch"]')) {
        add('check', `Set checkbox “${label}” to ${!checked(element) ? 'checked' : 'unchecked'}${suffix}`, {element, checked: !checked(element)});
      } else if (element instanceof HTMLSelectElement) {
        for (const option of element.options) if (!option.disabled && !option.parentElement?.disabled && option.value !== element.value)
          add('select', `Set “${label}” to “${option.text.slice(0, 300)}”${suffix}`, {element, value: option.value});
      } else if (editable(element)) {
        for (const [key, value] of Object.entries(values)) {
          if (value !== element.value) add('fill', `Fill “${label}” with supplied value “${key}”${suffix}`, {element, value}, key);
        }
      } else if (element.matches('button,a[href],input[type="button"],input[type="submit"],input[type="radio"],[role="button"],[role="radio"]')) {
        add('click', `Click “${label}”${suffix}`, {element});
      }
    }
    for (const [direction, amount] of [['down', 600], ['up', -600]]) add('scroll', `Scroll ${direction}`, {amount});
    add('wait', 'Wait briefly for the page');
    // Terminal choices are mandatory even on pages with no actionable elements.
    candidates.push({id: 'finish', kind: 'finish', label: 'Finish: the observed goal appears satisfied'});
    candidates.push({id: 'escalate', kind: 'escalate', label: 'Return to Codex: need planning, sign-in, unsupported controls or clarification'});
    snapshot = {id: snapshotId, origin: new URL(location.href).origin, signature: signature(list), list, actions};
    const states = list.map((element, index) => ({control: index, name: name(element), type: element.getAttribute('role') || element.type || element.tagName.toLowerCase(),
      disabled: Boolean(element.disabled || element.getAttribute('aria-disabled') === 'true'),
      checked: element.matches('input[type="checkbox"],[role="checkbox"],[role="switch"]') ? checked(element) : undefined,
      value: editable(element) || element instanceof HTMLSelectElement ? element.value.slice(0, 2000) : undefined}));
    return {snapshot_id: snapshot.id, url: location.href, title: document.title.slice(0, 1800),
      // Never export hidden HTML, scripts, password/OTP fields or browser storage.
      text: JSON.stringify({controls: states}).slice(0, 42000), candidates};
  }
  function prepare(id, choice) {
    if (!snapshot || id !== snapshot.id) return {rejected: 'Stale browser snapshot'};
    if (!safeToCapture(snapshot.origin)) return {rejected: 'Browser changed or authentication view requires human input'};
    let current;
    try { current = elements(); } catch { return {rejected: 'Page is no longer supported'}; }
    if (current.length !== snapshot.list.length || current.some((e, i) => e !== snapshot.list[i]) || signature(current) !== snapshot.signature)
      return {rejected: 'Page changed after observation; take a fresh observation'};
    const action = snapshot.actions.get(choice);
    if (!action) return {rejected: 'Unknown browser action'};
    if (action.element) {
      const element = action.element;
      if (!element.isConnected || !visible(element) || element.disabled || element.readOnly || forbidden(element)) return {rejected: 'Element no longer actionable'};
      const rect = element.getBoundingClientRect(), x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
      // Offscreen/covered controls need a new observation after scrolling.
      const root = element.getRootNode(), top = root.elementFromPoint(x, y);
      if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight || !(top === element || element.contains(top)))
        return {rejected: 'Element is offscreen or covered'};
      return {kind: action.kind, x, y};
    }
    return {kind: action.kind, amount: action.amount};
  }
  function fill(id, choice) {
    const action = snapshot?.id === id && snapshot.actions.get(choice);
    if (!action || action.kind !== 'fill') throw new Error('Unknown fill action');
    const element = action.element;
    if (document.activeElement !== element && element.getRootNode().activeElement !== element) throw new Error('Field is not focused');
    element.select();
    return action.value;
  }
  function select(id, choice) {
    const action = snapshot?.id === id && snapshot.actions.get(choice);
    if (!action || action.kind !== 'select') throw new Error('Unknown select action');
    const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set;
    setter.call(action.element, action.value);
    action.element.dispatchEvent(new Event('input', {bubbles: true}));
    action.element.dispatchEvent(new Event('change', {bubbles: true}));
  }
  function verify(id, choice) {
    const action = snapshot?.id === id && snapshot.actions.get(choice);
    if (!action) return false;
    if (action.kind === 'fill' || action.kind === 'select') return action.element.isConnected && action.element.value === action.value;
    if (action.kind === 'check') return action.element.isConnected && checked(action.element) === action.checked;
    return true; // Click dispatch is recorded; the next observation checks its result.
  }
  return {observe, prepare, fill, select, verify, safeToCapture};
})()
