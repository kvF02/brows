(async () => {
  const config = __BROWS_LOGIN_CONFIG__;
  const route = suffix => `/${suffix}/${config.round}/${config.account}`;
  addEventListener('pagehide', () => navigator.sendBeacon(route('closed'), 'closed'));
  const proof = {httpOnly: !document.cookie.includes(config.cookieName+'='), secureContext: isSecureContext};
  try {
    const database = await new Promise((resolve, reject) => {
      const request = indexedDB.open('brows-login-fixture', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('state');
      request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error);
    });
    const cache = await caches.open('brows-login-fixture');
    if (config.enroll) {
      localStorage.setItem('account', config.account);
      await new Promise((resolve, reject) => {
        const transaction = database.transaction('state', 'readwrite');
        transaction.objectStore('state').put(config.account, 'account');
        transaction.oncomplete = resolve; transaction.onerror = () => reject(transaction.error);
      });
      await cache.put('/account-marker', new Response(config.account));
    }
    proof.local = localStorage.getItem('account');
    proof.indexed = await new Promise((resolve, reject) => {
      const request = database.transaction('state').objectStore('state').get('account');
      request.onsuccess = () => resolve(request.result ?? null); request.onerror = () => reject(request.error);
    });
    const stored = await cache.match('/account-marker'); proof.cache = stored ? await stored.text() : null;
    database.close();
  } catch { proof.error = true; }
  await fetch(route('proof'), {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(proof)});
  document.body.dataset.ready = 'true';
})();
