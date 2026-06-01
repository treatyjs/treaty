// Standalone jest config for running the @treaty/ts-vite unit specs without the (environment-broken)
// `@nx/jest/preset`. Mirrors jest.config.ts's SWC transform, reading the same .swcrc.
const { readFileSync } = require('fs');
const { exclude: _drop, ...swcJestConfig } = JSON.parse(
  readFileSync(`${__dirname}/.swcrc`, 'utf-8'),
);
if (swcJestConfig.swcrc === undefined) {
  swcJestConfig.swcrc = false;
}

module.exports = {
  displayName: 'vite',
  rootDir: __dirname,
  transform: {
    '^.+\\.[tj]s$': ['@swc/jest', swcJestConfig],
  },
  moduleFileExtensions: ['ts', 'js', 'html'],
  testEnvironment: 'node',
  testMatch: ['<rootDir>/src/**/*.spec.ts'],
};
