import assert from 'node:assert/strict';
import test from 'node:test';
import {startLoginFixture} from './test_support/login_fixture.mjs';

test('read-only checks never silently authenticate a profile; enrolled cookies are secure and HttpOnly', async () => {
  const site = await startLoginFixture();
  try {
    const origin = site.origin.replace('localhost','127.0.0.1');
    const check = await fetch(origin+'/check/empty/a');
    assert.equal(check.headers.get('set-cookie'),null);
    const login = await fetch(origin+'/enroll/enroll/a');
    const cookie = login.headers.get('set-cookie');
    assert.match(cookie,/^__Host-brows-fixture=[a-f0-9]{64}; Path=\/; Max-Age=86400; Secure; HttpOnly; SameSite=Strict$/);
    const body = {local:'a',indexed:'a',cache:'a',httpOnly:true,secureContext:true};
    await fetch(origin+'/proof/enroll/a',{method:'POST',headers:{Cookie:cookie.split(';')[0]},body:JSON.stringify(body)});
    assert.equal(site.proofs['enroll/a'].account,'a');
    assert.equal(JSON.stringify(site.proofs).includes(cookie.split(';')[0]),false);
    await fetch(origin+'/proof/unauthenticated/b',{method:'POST',body:JSON.stringify({...body,local:null,indexed:null,cache:null})});
    assert.equal(site.proofs['unauthenticated/b'].account,null);
  } finally { await site.close(); }
});

test('proofs derive authentication from the request cookie, reject malformed storage and restrict routes', async () => {
  const site = await startLoginFixture();
  try {
    const origin = site.origin.replace('localhost','127.0.0.1');
    const spoof = await fetch(origin+'/proof/spoof/a',{method:'POST',body:JSON.stringify({account:'a',local:'a',indexed:'a',cache:'a',httpOnly:true,secureContext:true})});
    assert.equal(spoof.status,200); assert.equal(site.proofs['spoof/a'].account,null);
    const invalid = await fetch(origin+'/proof/invalid/a',{method:'POST',body:JSON.stringify({local:'arbitrary data'})});
    assert.equal(invalid.status,400); assert.equal(site.proofs['invalid/a'],undefined);
    assert.equal((await fetch(origin+'/enroll/invalid/c')).status,404);
    assert.equal((await fetch(origin+'/proof/large/a',{method:'POST',body:'x'.repeat(5000)})).status,413);
  } finally { await site.close(); }
});
