// Standalone jest config that bypasses the repo's Nx-based preset (the repo migrated to moon and
// `@nx/jest` is not installed). Uses jest-preset-angular directly with bundler module resolution
// plus the local elysia shim so the specs can run without the elysia peer installed.
const base = require('jest-preset-angular/jest-preset');

module.exports = {
  ...base,
  rootDir: __dirname,
  displayName: 'eden-client-standalone',
  setupFilesAfterEnv: ['<rootDir>/src/test-setup.standalone.ts'],
  testMatch: ['<rootDir>/src/**/*.spec.ts'],
  transform: {
    '^.+\\.(ts|mjs|js|html)$': [
      'jest-preset-angular',
      {
        tsconfig: '<rootDir>/tsconfig.spec.standalone.json',
        stringifyContentPathRegex: '\\.(html|svg)$',
        isolatedModules: true,
      },
    ],
  },
  transformIgnorePatterns: ['node_modules/(?!.*\\.mjs$)'],
};
