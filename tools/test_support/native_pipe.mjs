// Private transport for owned, isolated browser acceptance processes only.
import { EventEmitter } from 'node:events';
import { spawn } from 'node:child_process';

export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

export class NativePipe extends EventEmitter {
  constructor(child) {
    super();
    this.next = 0; this.pending = new Map(); this.bytes = Buffer.alloc(0);
    this.child = child;
    child.stdio[4].on('data', bytes => {
      this.bytes = Buffer.concat([this.bytes, bytes]);
      if (this.bytes.length > 2 * 1024 * 1024) return this.close(new Error('protocol frame limit'));
      let end;
      while ((end = this.bytes.indexOf(0)) >= 0) {
        const frame = this.bytes.subarray(0, end); this.bytes = this.bytes.subarray(end + 1);
        let value;
        try { value = JSON.parse(frame); } catch { return this.close(new Error('invalid protocol frame')); }
        if (value.id) {
          const waiter = this.pending.get(value.id);
          if (!waiter) continue;
          this.pending.delete(value.id); clearTimeout(waiter.timer);
          if (value.error) {
            const error = new Error(`${waiter.method}: protocol error ${value.error.code}`);
            error.code = value.error.code; waiter.reject(error);
          } else waiter.resolve(value.result);
        } else this.emit('event', value);
      }
    });
    for (const stream of [child.stdio[3], child.stdio[4]]) stream.on('error', () => this.close(new Error('protocol pipe closed')));
    child.on('exit', () => this.close(new Error('owned browser exited')));
    child.on('error', () => this.close(new Error('owned browser could not start')));
  }

  close(error) {
    this.closed ??= error;
    for (const waiter of this.pending.values()) { clearTimeout(waiter.timer); waiter.reject(this.closed); }
    this.pending.clear();
  }

  call(method, params = {}, sessionId) {
    if (this.closed) return Promise.reject(this.closed);
    if (this.pending.size >= 64) return Promise.reject(new Error('protocol pending limit'));
    return new Promise((resolve, reject) => {
      const id = ++this.next;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${method}: timeout`)); }, 8000);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdio[3].write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
    });
  }
}

export async function killOwned(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    await new Promise(resolve => {
      const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      killer.once('exit', resolve); killer.once('error', resolve);
    });
  } else { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }
  if (child.exitCode === null && child.signalCode === null) {
    await Promise.race([new Promise(resolve => child.once('exit', resolve)), sleep(5000)]);
  }
}
