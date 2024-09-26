import { CodeMapping, forEachEmbeddedCode, LanguagePlugin, VirtualCode } from '@volar/language-core';
import type { TypeScriptExtraServiceScript } from '@volar/typescript';
import type * as ts from 'typescript';
import * as html from 'vscode-html-languageservice';
import { URI } from 'vscode-uri';
import { AngularLanguageService } from '@angular/language-service';
import { Lexer } from './Lexer';

export const treatyLanguagePlugin: LanguagePlugin<URI> = {
	getLanguageId(uri) {
		if (uri.path.endsWith('.treaty')) {
			return 'treaty';
		}
	},
	createVirtualCode(_uri, languageId, snapshot) {
		if (languageId === 'treaty') {
			return new TreatyVirtualCode(snapshot);
		}
	},
	typescript: {
		extraFileExtensions: [{ extension: 'treaty', isMixedContent: true, scriptKind: 7 satisfies ts.ScriptKind.Deferred }],
		getServiceScript() {
			return AngularLanguageService;
		},
		getExtraServiceScripts(fileName, root) {
			const scripts: TypeScriptExtraServiceScript[] = [];
			for (const code of forEachEmbeddedCode(root)) {
				if (code.languageId === 'javascript') {
					scripts.push({
						fileName: fileName + '.' + code.id + '.js',
						code,
						extension: '.js',
						scriptKind: 1 satisfies ts.ScriptKind.JS,
					});
				}
				else if (code.languageId === 'typescript') {
					scripts.push({
						fileName: fileName + '.' + code.id + '.ts',
						code,
						extension: '.ts',
						scriptKind: 3 satisfies ts.ScriptKind.TS,
					});
				}
			}
			return scripts;
		},
	},
};


export class TreatyVirtualCode implements VirtualCode {
	id = 'root';
	languageId = 'treaty';
	mappings: CodeMapping[];
	embeddedCodes: VirtualCode[] = [];

	constructor(public snapshot: ts.IScriptSnapshot) {
		const lexer = new Lexer(snapshot.getText(0, snapshot.getLength()));
		this.mappings = lexer.getMappings();
		this.embeddedCodes = lexer.getEmbeddedCodes();
	}
}
