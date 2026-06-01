/* eslint-disable */
// Standalone Jest config for the linker unit suite that does NOT depend on the
// (now-removed) @nx/jest preset. Mirrors jest.config.ts's SWC transform.
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
};
