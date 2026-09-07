export function createAuthenticationGate(probe) {
  let authenticated = false;
  let pending = null;
  let failure = null;

  return {
    async wait() {
      if (authenticated) return;
      if (failure) throw failure;
      if (!pending) {
        pending = Promise.resolve()
          .then(probe)
          .then(() => {
            authenticated = true;
          })
          .catch((error) => {
            failure = error;
            throw error;
          })
          .finally(() => {
            pending = null;
          });
      }
      return pending;
    },
    block(error) {
      authenticated = false;
      pending = null;
      failure = error;
    },
  };
}
