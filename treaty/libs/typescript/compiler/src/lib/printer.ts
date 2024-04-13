/**
 * @license
 * Copyright Google LLC All Rights Reserved.
 *
 * Use of this source code is governed by an MIT-style license that can be
 * found in the LICENSE file at https://angular.io/license
 */

// A Huge thanks to Alex Rickabaugh for writing this 🙏.

import type * as ng from '@angular/compiler';

//@ts-ignore
export function Printer(ng: typeof import('@angular/compiler')) {

  const UNARY_OPERATORS = new Map<ng.UnaryOperator, string>([
    [ng.UnaryOperator.Minus, '-'],
    [ng.UnaryOperator.Plus, '+'],
  ]);

  const BINARY_OPERATORS = new Map<ng.BinaryOperator, string>([
    [ng.BinaryOperator.And, '&&'],
    [ng.BinaryOperator.Bigger, '>'],
    [ng.BinaryOperator.BiggerEquals, '>='],
    [ng.BinaryOperator.BitwiseAnd, '&'],
    [ng.BinaryOperator.BitwiseOr, '|'],
    [ng.BinaryOperator.Divide, '/'],
    [ng.BinaryOperator.Equals, '=='],
    [ng.BinaryOperator.Identical, '==='],
    [ng.BinaryOperator.Lower, '<'],
    [ng.BinaryOperator.LowerEquals, '<='],
    [ng.BinaryOperator.Minus, '-'],
    [ng.BinaryOperator.Modulo, '%'],
    [ng.BinaryOperator.Multiply, '*'],
    [ng.BinaryOperator.NotEquals, '!='],
    [ng.BinaryOperator.NotIdentical, '!=='],
    [ng.BinaryOperator.Or, '||'],
    [ng.BinaryOperator.Plus, '+'],
    [ng.BinaryOperator.NullishCoalesce, '??'],
  ]);

  class Context {
    constructor(readonly isStatement: boolean) { }

    get withExpressionMode(): Context {
      return this.isStatement ? new Context(false) : this;
    }

    get withStatementMode(): Context {
      return !this.isStatement ? new Context(true) : this;
    }
  }

  class Printer implements ng.ExpressionVisitor, ng.StatementVisitor {
    visitDeclareVarStmt(stmt: ng.DeclareVarStmt, context: Context): string {
      let varStmt = stmt.hasModifier(ng.StmtModifier.Final) ? 'const' : 'let';
      varStmt += ' ' + stmt.name;
      if (stmt.value) {
        varStmt +=
          ' = ' + stmt.value.visitExpression(this, context.withExpressionMode);
      }
      return this.attachComments(varStmt, stmt.leadingComments);
    }

    visitDeclareFunctionStmt(
      stmt: ng.DeclareFunctionStmt,
      context: Context
    ): string {
      let fn = `function ${stmt.name}(${stmt.params
        .map((p) => p.name)
        .join(', ')}) {`;
      fn += this.visitStatements(stmt.statements, context.withStatementMode);
      fn += '}';
      return this.attachComments(fn, stmt.leadingComments);
    }

    visitExpressionStmt(stmt: ng.ExpressionStatement, context: Context): string {
      return this.attachComments(
        stmt.expr.visitExpression(this, context.withStatementMode) + ';',
        stmt.leadingComments
      );
    }

    visitReturnStmt(stmt: ng.ReturnStatement, context: Context): string {
      return this.attachComments(
        'return ' +
        stmt.value.visitExpression(this, context.withExpressionMode) +
        ';',
        stmt.leadingComments
      );
    }

    visitIfStmt(stmt: ng.IfStmt, context: Context): string {
      let ifStmt = 'if (';
      ifStmt += stmt.condition.visitExpression(this, context);
      ifStmt +=
        ') {' +
        this.visitStatements(stmt.trueCase, context.withStatementMode) +
        '}';
      if (stmt.falseCase.length > 0) {
        ifStmt += ' else {';
        ifStmt += this.visitStatements(stmt.falseCase, context.withStatementMode);
        ifStmt += '}';
      }
      return this.attachComments(ifStmt, stmt.leadingComments);
    }

    visitReadVarExpr(ast: ng.ReadVarExpr, _context: Context): string {
      return ast.name;
    }

    visitWriteVarExpr(expr: ng.WriteVarExpr, context: Context): string {
      const assignment = `${expr.name} = ${expr.value.visitExpression(
        this,
        context
      )}`;
      return context.isStatement ? assignment : `(${assignment})`;
    }

    visitWriteKeyExpr(expr: ng.WriteKeyExpr, context: Context): string {
      const exprContext = context.withExpressionMode;
      const receiver = expr.receiver.visitExpression(this, exprContext);
      const key = expr.index.visitExpression(this, exprContext);
      const value = expr.value.visitExpression(this, exprContext);
      const assignment = `${receiver}[${key}] = ${value}`;
      return context.isStatement ? assignment : `(${assignment})`;
    }

    visitWritePropExpr(expr: ng.WritePropExpr, context: Context): string {
      const receiver = expr.receiver.visitExpression(this, context);
      const value = expr.value.visitExpression(this, context);
      return `${receiver}.${expr.name} = ${value}`;
    }

    visitInvokeFunctionExpr(
      ast: ng.InvokeFunctionExpr,
      context: Context
    ): string {
      const fn = ast.fn.visitExpression(this, context);
      const args = ast.args.map((arg) => {
        return arg.visitExpression(this, context)
      });
      return this.setSourceMapRange(

        `${fn}(${args.join(', ')})`,
        ast.sourceSpan
      );
    }

    visitTaggedTemplateExpr(
      ast: ng.TaggedTemplateExpr,
      context: Context
    ): string {
      throw new Error('only important for i18n');

    }

    visitInstantiateExpr(ast: ng.InstantiateExpr, context: Context): string {
      const ctor = ast.classExpr.visitExpression(this, context);
      const args = ast.args.map((arg) => arg.visitExpression(this, context));
      return `new ${ctor}(${args.join(', ')})`;
    }

    visitLiteralExpr(ast: ng.LiteralExpr, _context: Context): string {
      let value: string;
      if (typeof ast.value === 'string') {
        value = `'` + ast.value.replaceAll(`'`, `\\'`) + `'`;
      } else if (ast.value === undefined) {
        value = 'undefined';
      } else if (ast.value === null) {
        value = 'null';
      } else {
        value = ast.value.toString();
      }
      return this.setSourceMapRange(value, ast.sourceSpan);
    }

    visitLocalizedString(ast: ng.LocalizedString, context: Context): string {
      throw new Error('only important for i18n');

    }

    visitExternalExpr(ast: ng.ExternalExpr, _context: Context): string {
      if (ast.value.name === null) {
        if (ast.value.moduleName === null) {
          throw new Error('Invalid import without name nor moduleName');
        }
        return 'i0';
      }

      if (ast.value.moduleName !== null) {
        return `i0.${ast.value.name}`;
      } else {

        return ast.value.name;
      }
    }

    visitConditionalExpr(ast: ng.ConditionalExpr, context: Context): string {
      let cond: string = ast.condition.visitExpression(this, context);

      if (ast.condition instanceof ng.ConditionalExpr) {

        cond = `(${cond})`;
      }

      return (
        cond +
        ' ? ' +
        ast.trueCase.visitExpression(this, context) +
        ' : ' +
        ast.falseCase!.visitExpression(this, context)
      );
    }

    visitDynamicImportExpr(ast: ng.DynamicImportExpr, context: any) {
      return `import('${ast.url}')`;
    }

    visitNotExpr(ast: ng.NotExpr, context: Context): string {
      return '!' + ast.condition.visitExpression(this, context);
    }

    visitFunctionExpr(ast: ng.FunctionExpr, context: Context): string {
      let fn = `function `;
      if (ast.name) {
        fn += ast.name;
      }
      fn += `(` + ast.params.map((param) => param.name).join(', ') + ') {';
      fn += this.visitStatements(ast.statements, context);
      fn += '}';
      return fn;
    }

    visitArrowFunctionExpr(ast: ng.ArrowFunctionExpr, context: any) {
      const params = ast.params.map((param) => param.name).join(', ');
      let body: string;
      if (Array.isArray(ast.body)) {
        body = '{' + this.visitStatements(ast.body, context) + '}';
      } else {
        body = ast.body.visitExpression(this, context);
      }
      return `(${params}) => ${body}`;
    }

    visitBinaryOperatorExpr(
      ast: ng.BinaryOperatorExpr,
      context: Context
    ): string {
      if (!BINARY_OPERATORS.has(ast.operator)) {
        throw new Error(
          `Unknown binary operator: ${ng.BinaryOperator[ast.operator]}`
        );
      }
      return (
        ast.lhs.visitExpression(this, context) +
        BINARY_OPERATORS.get(ast.operator)! +
        ast.rhs.visitExpression(this, context)
      );
    }

    visitReadPropExpr(ast: ng.ReadPropExpr, context: Context): string {
      return ast.receiver.visitExpression(this, context) + '.' + ast.name;
    }

    visitReadKeyExpr(ast: ng.ReadKeyExpr, context: Context): string {
      const receiver = ast.receiver.visitExpression(this, context);
      const key = ast.index.visitExpression(this, context);
      return `${receiver}[${key}]`;
    }

    visitLiteralArrayExpr(ast: ng.LiteralArrayExpr, context: Context): string {
      const entries = ast.entries.map((expr) =>
        this.setSourceMapRange(
          expr.visitExpression(this, context),
          ast.sourceSpan
        )
      );
      return '[' + entries.join(', ') + ']';
    }

    visitLiteralMapExpr(ast: ng.LiteralMapExpr, context: Context): string {
      const properties: string[] = ast.entries.map((entry) => {
        let key = entry.key;
        if (entry.quoted) {
          key = `'` + key.replaceAll(`'`, `\\'`) + `'`;
        }
        return key + ': ' + entry.value.visitExpression(this, context);
      });
      return this.setSourceMapRange(
        '{' + properties.join(', ') + '}',
        ast.sourceSpan
      );
    }

    visitCommaExpr(ast: ng.CommaExpr, context: Context): never {
      throw new Error('Method not implemented.');
    }

    visitWrappedNodeExpr(
      ast: ng.WrappedNodeExpr<string>,
      _context: Context
    ): string {
      return ast.node;
    }

    visitTypeofExpr(ast: ng.TypeofExpr, context: Context): string {
      return 'typeof ' + ast.expr.visitExpression(this, context);
    }

    visitUnaryOperatorExpr(ast: ng.UnaryOperatorExpr, context: Context): string {
      if (!UNARY_OPERATORS.has(ast.operator)) {
        throw new Error(
          `Unknown unary operator: ${ng.UnaryOperator[ast.operator]}`
        );
      }
      return (
        UNARY_OPERATORS.get(ast.operator)! +
        ast.expr.visitExpression(this, context)
      );
    }

    private visitStatements(
      statements: ng.Statement[],
      context: Context
    ): string {
      return statements
        .map((stmt) => stmt.visitStatement(this, context))
        .filter((stmt) => stmt !== undefined)
        .join('\n');
    }

    private setSourceMapRange(
      ast: string,
      span: ng.ParseSourceSpan | null
    ): string {
      return ast;
    }

    private attachComments(
      statement: string,
      leadingComments: ng.LeadingComment[] | undefined
    ): string {

      return statement;
    }
  }

  return { Context, Printer };
}