export class SessionMessageCache {
  constructor(limit = 3) {
    this.limit = limit;
    this.entries = new Map();
  }

  get(threadId) {
    const value = this.entries.get(threadId);
    if (value === undefined) return null;
    this.entries.delete(threadId);
    this.entries.set(threadId, value);
    return value;
  }

  set(threadId, value) {
    this.entries.delete(threadId);
    this.entries.set(threadId, value);
    while (this.entries.size > this.limit) {
      this.entries.delete(this.entries.keys().next().value);
    }
  }

  delete(threadId) {
    return this.entries.delete(threadId);
  }
}
