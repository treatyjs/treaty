import { ComponentFixture, TestBed, getTestBed } from '@angular/core/testing';
import { beforeEach, describe, expect, test } from 'bun:test';
import PostComponent from './post.component';

describe('PostComponent', () => {
  let component: PostComponent;
  let fixture: ComponentFixture<PostComponent>;

  beforeEach(async () => {
    await getTestBed()
      .configureTestingModule({
        imports: [PostComponent],
      })
      .compileComponents();

    fixture = TestBed.createComponent(PostComponent);
    component = fixture.componentInstance;
    // component.post = input('test');

    // Could use some improved mocking of treaty here
    (component as any).api = {
      client: {
        id: {
          test: {
            get: async () => ({ data: 'test' }),
          },
        },
      },
    };

    // component.post = input('test');

    await fixture.whenStable();
  });

  test('should create the app', () => {
    expect(component).toBeTruthy();
  });

  test(`should have as title 'treaty'`, () => {
    expect(component.title).toEqual('treaty');
  });
});
