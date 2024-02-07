import { TestBed } from '@angular/core/testing';
import { RouterTestingModule } from '@angular/router/testing';
import { beforeEach, describe, expect, test } from 'bun:test';
import PostComponent from './post.component';

describe('PostComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [RouterTestingModule, PostComponent],
    }).compileComponents();
  });

  test('should create the app', () => {
    const fixture = TestBed.createComponent(PostComponent);
    const app = fixture.componentInstance;
    expect(app).toBeTruthy();
  });

  test(`should have as title 'treaty'`, () => {
    const fixture = TestBed.createComponent(PostComponent);
    const app = fixture.componentInstance;
    expect(app.title).toEqual('treaty');
  });
});
