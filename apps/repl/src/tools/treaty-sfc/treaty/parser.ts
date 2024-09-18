import { Token, ControlFlowKind, DeferKind, TokenType } from './token';

interface AstNode {
    type: TokenType;
    value: string;
}

interface Ast {
    nodes: AstNode[];
}

class Parser {
    private tokens: Token[];
    private pos: number;

    constructor(tokens: Token[]) {
        this.tokens = tokens;
        this.pos = 0;
    }

    parse(): Ast {
        const nodes: AstNode[] = [];

        while (!this.isAtEnd()) {
            const node = this.parseNode();
            nodes.push(node);
        }

        return { nodes };
    }

    private parseNode(): AstNode {
        const token = this.advance();

        switch (token.kind.type) {
            case TokenType.JavaScript:
                return { type: TokenType.JavaScript, value: token.kind.value };
            case TokenType.Style:
                return { type: TokenType.Style, value: token.kind.value };
            case TokenType.HTML:
                return { type: TokenType.HTML, value: token.kind.value };
            case TokenType.TemplateExpression:
                return { type: TokenType.HTML, value: token.kind.value };
            case TokenType.ControlFlow:
                return this.parseControlFlowNode(token.kind.kind);
            case TokenType.Defer:
                return this.parseDeferNode(token.kind.kind);
            case 'Eof':
                return { type: 'EOF', value: '' };
            default:
                throw new Error(`Unexpected token type: ${token.kind}`);
        }
    }

    private parseControlFlowNode(kind: ControlFlowKind): AstNode {
        switch (kind) {
            case ControlFlowKind.If:
                return { type: TokenType.HTML, value: '@if' };
            case ControlFlowKind.ElseIf:
                return { type: TokenType.HTML, value: '@else if' };
            case ControlFlowKind.Else:
                return { type: TokenType.HTML, value: '@else' };
            case ControlFlowKind.For:
                return { type: TokenType.HTML, value: '@for' };
            case ControlFlowKind.Empty:
                return { type: TokenType.HTML, value: '@empty' };
            case ControlFlowKind.Switch:
                return { type: TokenType.HTML, value: '@switch' };
            case ControlFlowKind.Case:
                return { type: TokenType.HTML, value: '@case' };
            case ControlFlowKind.Default:
                return { type: TokenType.HTML, value: '@default' };
        }
    }

    private parseDeferNode(kind: DeferKind): AstNode {
        switch (kind) {
            case DeferKind.Defer:
                return { type: TokenType.HTML, value: '@defer' };
            case DeferKind.Placeholder:
                return { type: TokenType.HTML, value: '@placeholder' };
            case DeferKind.Loading:
                return { type: TokenType.HTML, value: '@loading' };
            case DeferKind.Error:
                return { type: TokenType.HTML, value: '@error' };
        }
    }
    private advance(): Token {
        if (!this.isAtEnd()) {
            this.pos++;
        }
        return this.previous();
    }

    private isAtEnd(): boolean {
        return this.peek().kind.type === 'Eof';
    }

    private peek(): Token {
        return this.tokens[this.pos] || { kind: { type: 'Eof' }, start: 0, end: 0 };
    }

    private previous(): Token {
        return this.tokens[this.pos - 1];
    }
}

export { Parser, type AstNode, type Ast };