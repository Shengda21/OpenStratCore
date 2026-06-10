#!/usr/bin/env python3
"""Self-play PPO for openstratcore (single-file, CleanRL-style).

Runs out of the box on the pure-Python mock backend:
    python examples/selfplay_ppo.py --backend mock --total-steps 50000

To train against the real engine once mechanics land:
    make py-dev        # build the openstratcore_core extension
    python examples/selfplay_ppo.py --backend rust --total-steps 200000

The two sides ('red','blue') share one policy network -> self-play. Swap in a
different algorithm by editing this file or adding examples/<algo>.py; the env API
(reset/step over two agents) stays fixed.
"""
from __future__ import annotations

import argparse
import os
import time
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.optim as optim
from torch.distributions import Categorical

from openstratcore_env import make_env
from openstratcore_env.mock_backend import AGENTS, N_ACTIONS, OBS_DIM


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--backend", default="mock", choices=["mock", "rust"])
    p.add_argument("--opponent", default="self", choices=["self", "scripted"],
                   help="'self' = both sides share the policy (self-play); 'scripted' = blue is the "
                        "in-repo ScriptedCommander and only red learns (rust backend only)")
    p.add_argument("--total-steps", type=int, default=50_000)
    p.add_argument("--num-steps", type=int, default=256, help="rollout length per update")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--lr", type=float, default=2.5e-4)
    p.add_argument("--gamma", type=float, default=0.99)
    p.add_argument("--gae-lambda", type=float, default=0.95)
    p.add_argument("--clip", type=float, default=0.2)
    p.add_argument("--epochs", type=int, default=4)
    p.add_argument("--minibatches", type=int, default=4)
    p.add_argument("--ent-coef", type=float, default=0.01)
    p.add_argument("--vf-coef", type=float, default=0.5)
    p.add_argument("--max-grad-norm", type=float, default=0.5)
    p.add_argument("--save", default="runs", help="checkpoint dir")
    return p.parse_args()


class ActorCritic(nn.Module):
    def __init__(self, obs_dim: int, n_actions: int, hidden: int = 64):
        super().__init__()
        self.trunk = nn.Sequential(
            nn.Linear(obs_dim, hidden), nn.Tanh(),
            nn.Linear(hidden, hidden), nn.Tanh(),
        )
        self.pi = nn.Linear(hidden, n_actions)
        self.v = nn.Linear(hidden, 1)

    def forward(self, x):
        h = self.trunk(x)
        return self.pi(h), self.v(h).squeeze(-1)

    def act(self, x):
        logits, value = self.forward(x)
        dist = Categorical(logits=logits)
        a = dist.sample()
        return a, dist.log_prob(a), dist.entropy(), value

    def evaluate(self, x, a):
        logits, value = self.forward(x)
        dist = Categorical(logits=logits)
        return dist.log_prob(a), dist.entropy(), value


def stack_obs(obs_dict, agents):
    return np.stack([obs_dict[a] for a in agents], axis=0)


def main():
    args = parse_args()
    torch.manual_seed(args.seed)
    np.random.seed(args.seed)
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")

    env = make_env(backend=args.backend, seed=args.seed, opponent=args.opponent)
    learners = list(env.learners)   # agents the policy controls: ['red','blue'] self-play, ['red'] scripted
    n_agents = len(learners)        # the batch/parallel dimension (2 self-play, 1 vs scripted)
    print(f"backend={args.backend} opponent={args.opponent} learners={learners}")

    agent = ActorCritic(OBS_DIM, N_ACTIONS).to(device)
    opt = optim.Adam(agent.parameters(), lr=args.lr, eps=1e-5)

    batch = args.num_steps * n_agents
    minibatch_size = batch // args.minibatches

    obs_buf = torch.zeros((args.num_steps, n_agents, OBS_DIM), device=device)
    act_buf = torch.zeros((args.num_steps, n_agents), dtype=torch.long, device=device)
    logp_buf = torch.zeros((args.num_steps, n_agents), device=device)
    rew_buf = torch.zeros((args.num_steps, n_agents), device=device)
    done_buf = torch.zeros((args.num_steps, n_agents), device=device)
    val_buf = torch.zeros((args.num_steps, n_agents), device=device)

    obs_dict, _ = env.reset(seed=args.seed)
    next_obs = torch.tensor(stack_obs(obs_dict, learners), dtype=torch.float32, device=device)
    next_done = torch.zeros(n_agents, device=device)

    ep_returns: list[float] = []
    ep_lengths: list[int] = []
    ep_winners: list[str | None] = []     # 'red' / 'blue' / None(draw) — was the standoff ever broken?
    ep_terminated: list[bool] = []        # True if ended by win, False if hit the truncation cap
    ep_diags: list[dict] = []             # per-episode {accepted, rejected, fires, hits}
    running = np.zeros(n_agents)
    ep_len = 0
    ep_acc = {"accepted": 0, "rejected": 0, "fires": 0, "hits": 0}
    global_step = 0
    updates = max(1, args.total_steps // batch)
    start = time.time()

    for update in range(1, updates + 1):
        for step in range(args.num_steps):
            global_step += n_agents
            obs_buf[step] = next_obs
            done_buf[step] = next_done

            with torch.no_grad():
                a, logp, _, v = agent.act(next_obs)
            act_buf[step] = a
            logp_buf[step] = logp
            val_buf[step] = v

            actions = {ag: int(a[i].item()) for i, ag in enumerate(learners)}
            obs_dict, rewards, terms, truncs, infos = env.step(actions)
            r = np.array([rewards[ag] for ag in learners], dtype=np.float32)
            d = np.array([float(terms[ag] or truncs[ag]) for ag in learners], dtype=np.float32)
            rew_buf[step] = torch.tensor(r, device=device)
            running += r

            # Accumulate per-episode diagnostics from infos (observational; rust backend fills all keys,
            # mock fills fires/hits). Read BOTH sides regardless of who learns: accepted/rejected per-side
            # → sum; fires/hits are global → read once; winner is set identically on both.
            ep_len += 1
            ir, ib = infos[AGENTS[0]], infos[AGENTS[1]]
            ep_acc["accepted"] += ir.get("accepted", 0) + ib.get("accepted", 0)
            ep_acc["rejected"] += ir.get("rejected", 0) + ib.get("rejected", 0)
            ep_acc["fires"] += ir.get("fires", 0)
            ep_acc["hits"] += ir.get("hits", 0)
            won = bool(terms[AGENTS[0]] or terms[AGENTS[1]])  # terminated by victory vs truncation cap

            if d.all():  # episode over -> log and reset
                ep_returns.append(float(running.mean()))
                ep_lengths.append(ep_len)
                ep_winners.append(ir.get("winner"))
                ep_terminated.append(won)
                ep_diags.append(ep_acc)
                running = np.zeros(n_agents)
                ep_len = 0
                ep_acc = {"accepted": 0, "rejected": 0, "fires": 0, "hits": 0}
                obs_dict, _ = env.reset()
                d = np.ones(n_agents, dtype=np.float32)

            next_obs = torch.tensor(stack_obs(obs_dict, learners), dtype=torch.float32, device=device)
            next_done = torch.tensor(d, device=device)

        # GAE -----------------------------------------------------------------
        with torch.no_grad():
            _, next_value = agent.forward(next_obs)
            adv = torch.zeros_like(rew_buf)
            last_gae = torch.zeros(n_agents, device=device)
            for t in reversed(range(args.num_steps)):
                nonterminal = 1.0 - (next_done if t == args.num_steps - 1 else done_buf[t + 1])
                next_val = next_value if t == args.num_steps - 1 else val_buf[t + 1]
                delta = rew_buf[t] + args.gamma * next_val * nonterminal - val_buf[t]
                last_gae = delta + args.gamma * args.gae_lambda * nonterminal * last_gae
                adv[t] = last_gae
            returns = adv + val_buf

        b_obs = obs_buf.reshape(-1, OBS_DIM)
        b_act = act_buf.reshape(-1)
        b_logp = logp_buf.reshape(-1)
        b_adv = adv.reshape(-1)
        b_ret = returns.reshape(-1)
        b_adv = (b_adv - b_adv.mean()) / (b_adv.std() + 1e-8)

        idx = np.arange(batch)
        for _ in range(args.epochs):
            np.random.shuffle(idx)
            for start_i in range(0, batch, minibatch_size):
                mb = idx[start_i:start_i + minibatch_size]
                new_logp, entropy, value = agent.evaluate(b_obs[mb], b_act[mb])
                ratio = (new_logp - b_logp[mb]).exp()
                pg1 = -b_adv[mb] * ratio
                pg2 = -b_adv[mb] * torch.clamp(ratio, 1 - args.clip, 1 + args.clip)
                pg_loss = torch.max(pg1, pg2).mean()
                v_loss = 0.5 * ((value - b_ret[mb]) ** 2).mean()
                loss = pg_loss - args.ent_coef * entropy.mean() + args.vf_coef * v_loss
                opt.zero_grad()
                loss.backward()
                nn.utils.clip_grad_norm_(agent.parameters(), args.max_grad_norm)
                opt.step()

        if ep_returns:
            recent = np.mean(ep_returns[-20:])
            sps = int(global_step / (time.time() - start))
            w = ep_winners[-20:]
            red, blue = w.count("red"), w.count("blue")
            draw = len(w) - red - blue
            mlen = np.mean(ep_lengths[-20:])
            term_rate = float(np.mean(ep_terminated[-20:]))  # frac ended by win, not truncation
            dg = ep_diags[-20:]
            acc = sum(x["accepted"] for x in dg)
            rej = sum(x["rejected"] for x in dg)
            acc_rate = acc / (acc + rej) if (acc + rej) > 0 else float("nan")
            fires = sum(x["fires"] for x in dg)
            hits = sum(x["hits"] for x in dg)
            # Diagnostics answer: does the policy ever fight (fires/hits), break the standoff (W r/b/draw,
            # term%), and issue valid orders (acc%) — or is the flat return just no-op spam until truncation?
            print(f"update {update}/{updates}  step {global_step}  "
                  f"ep_return(mean20) {recent:+.2f}  sps {sps}  "
                  f"W[r{red}/b{blue}/d{draw}]  len {mlen:.0f}  term {term_rate:.0%}  "
                  f"acc {acc_rate:.0%}  fires {fires}  hits {hits}")

    out = Path(args.save)
    out.mkdir(parents=True, exist_ok=True)
    ckpt = out / "selfplay_ppo.pt"
    torch.save(agent.state_dict(), ckpt)
    print(f"saved checkpoint -> {ckpt}")


if __name__ == "__main__":
    main()
