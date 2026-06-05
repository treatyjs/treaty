import { describe, expect, it } from 'vitest'

import { parseChangedFiles } from '../src/index.js'

describe('parseChangedFiles', () => {
	it('parses a git diff --name-only listing into sorted unique paths', () => {
		const text = 'libs/ui/button.ts\nsrc/app/main.ts\nlibs/ui/button.ts\n'
		expect(parseChangedFiles(text)).toEqual(['libs/ui/button.ts', 'src/app/main.ts'])
	})

	it('handles CRLF, blank lines, and NUL separators', () => {
		expect(parseChangedFiles('a.ts\r\n\r\nb.ts')).toEqual(['a.ts', 'b.ts'])
		expect(parseChangedFiles('a.ts\0b.ts\0')).toEqual(['a.ts', 'b.ts'])
	})

	it('strips git quoting and normalizes backslashes', () => {
		expect(parseChangedFiles('"libs/with space/x.ts"')).toEqual(['libs/with space/x.ts'])
		expect(parseChangedFiles('libs\\ui\\x.ts')).toEqual(['libs/ui/x.ts'])
	})

	it('returns empty for empty input', () => {
		expect(parseChangedFiles('')).toEqual([])
		expect(parseChangedFiles('\n\n  \n')).toEqual([])
	})
})
