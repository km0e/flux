/**
 * skills.test.ts — the Skills-management client over Connect: delegation
 * plus the store write the `skills` broadcast owns. (The trim/normalize
 * of the install sources lives in core/grpc's call helpers.)
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useFlux, resetFluxForTest } from '../../core/state';
import { addSkill, removeSkill, fetchSkills, handleSkillsMessage } from '../../services/skills';
import * as grpc from '../../core/grpc';

vi.mock('../../core/grpc', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../core/grpc')>()),
  grpcFetchSkills: vi.fn(async () => undefined),
  grpcAddSkill: vi.fn(async () => undefined),
  grpcRemoveSkill: vi.fn(async () => undefined),
}));

describe('skills service', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
  });

  it('fetchSkills delegates with the active chat', () => {
    fetchSkills('c1');
    expect(vi.mocked(grpc.grpcFetchSkills)).toHaveBeenCalledWith('c1');
    fetchSkills();
    expect(vi.mocked(grpc.grpcFetchSkills)).toHaveBeenLastCalledWith(undefined);
  });

  it('addSkill delegates exactly one source', async () => {
    await addSkill({ path: ' /tmp/myskill ', url: undefined, subpath: undefined });
    expect(vi.mocked(grpc.grpcAddSkill)).toHaveBeenCalledWith({
      path: ' /tmp/myskill ',
      url: undefined,
      subpath: undefined,
    });
    vi.mocked(grpc.grpcAddSkill).mockResolvedValueOnce('name collision');
    await expect(addSkill({ url: 'https://github.com/u/skills' })).resolves.toBe('name collision');
  });

  it('removeSkill delegates the inline error; project-local names included', async () => {
    vi.mocked(grpc.grpcRemoveSkill).mockResolvedValueOnce('unknown skill');
    await expect(removeSkill('nope')).resolves.toBe('unknown skill');
  });

  it('handleSkillsMessage fills the store (the broadcast handler)', () => {
    handleSkillsMessage([{ name: 'pdf', description: 'd', source: 'global', removable: true }]);
    expect(useFlux.getState().skills).toHaveLength(1);
  });
});
