/**
 * model-section.test.tsx — the saved-model row surfaces the models.dev
 * match: the badge tooltip names the matched catalog entry, and an
 * ALIASED match (catalog id ≠ saved id — spelling/snapshot-suffix
 * normalization) is shown inline, never silent. A meta-less row shows
 * neither.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { SavedModelsSection } from '../../components/settings/ModelSection';
import { useFlux, resetFluxForTest } from '../../core/state';

vi.mock('../../services/models', () => ({
  saveModel: vi.fn(),
  removeModel: vi.fn(),
  syncModels: vi.fn(),
}));

describe('SavedModelsSection match display', () => {
  beforeEach(() => {
    resetFluxForTest();
    document.body.innerHTML = '';
  });

  it('an exact match shows the badge without an alias line', () => {
    useFlux.setState({
      savedModels: [
        {
          provider: 'zai',
          model: 'glm-5.3-flash',
          params: {},
          meta: {
            source: 'models.dev',
            name: 'GLM-5.3 Flash',
            models_dev: { provider: 'zai', model: 'glm-5.3-flash' },
          },
        },
      ],
    });
    render(<SavedModelsSection provider="zai" />);
    expect(screen.getByText('glm-5.3-flash')).toBeTruthy();
    expect(screen.getByText('models.dev')).toBeTruthy();
    expect(screen.queryByText(/matched:/)).toBeNull();
  });

  it('an aliased match names the catalog entry inline and in the tooltip', () => {
    useFlux.setState({
      savedModels: [
        {
          provider: 'zai',
          model: 'glm-5-3-flash-260828',
          params: {},
          meta: {
            source: 'models.dev',
            name: 'GLM-5.3 Flash',
            models_dev: { provider: 'zai', model: 'glm-5.3-flash' },
          },
        },
      ],
    });
    render(<SavedModelsSection provider="zai" />);
    expect(screen.getByText(/matched: zai\/glm-5\.3-flash/)).toBeTruthy();
    // The badge tooltip names the match too (works without hovering).
    expect((screen.getByText('models.dev') as HTMLElement).title).toMatch(
      /matched zai\/glm-5\.3-flash/,
    );
  });

  it('a row without a models.dev snapshot shows no badge and no alias', () => {
    useFlux.setState({
      savedModels: [
        { provider: 'zai', model: 'glm-5-3-flash-260828', params: {}, meta: {} },
      ],
    });
    render(<SavedModelsSection provider="zai" />);
    expect(screen.getByText('glm-5-3-flash-260828')).toBeTruthy();
    expect(screen.queryByText('models.dev')).toBeNull();
    expect(screen.queryByText(/matched:/)).toBeNull();
  });
});
