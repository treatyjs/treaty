import { ɵprovideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { beforeEach, describe, expect, test } from 'bun:test';
import PostComponent from './post.component';

describe('PostComponent', () => {
  let component: PostComponent;
  let fixture: ComponentFixture<PostComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [PostComponent],
      providers: [provideRouter([]), ɵprovideZonelessChangeDetection()],
    }).compileComponents();

    fixture = TestBed.createComponent(PostComponent);
    component = fixture.componentInstance;
    fixture.componentRef.setInput('id', 'test');
    await fixture.whenStable();
  });

  test('should create the app', () => {
    const app = fixture.componentInstance;
    expect(app).toBeTruthy();
  });

  test(`should have as title 'treaty'`, () => {
    const app = fixture.componentInstance;
    expect(app.title).toEqual('treaty');
  });
});
