if (typeof (globalThis as any).Zone === 'undefined') {
    const mockZone = {
      current: {
        get: (key: string): boolean | undefined =>
          key === 'isAngularZone' ? true : undefined,
        run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T {
          return fn.apply(applyThis, applyArgs);
        },
      },
      run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T {
        return fn.apply(applyThis, applyArgs);
      },
    };
    (globalThis as any).Zone = mockZone;
  }