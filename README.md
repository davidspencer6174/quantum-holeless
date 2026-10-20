The goal is to find the longest holeless level within a given rectangle in the game [Pathology](https://pathology.thinky.gg/). Most of the code is LLM-written, but this README is fully human-written.

# Fundamental Algorithm

This is a variant of the classic backsolver-style algorithm: community parlance for exhaustive searches that start with the player on the exit and walls elsewhere, and make all possible "undo" moves to search backward for the longest level. Walls signify that the intended forward solve doesn't touch that tile, so there's no need for it to be anything other than a wall. Undo moves can either be undo-walks or undo-pushes (undo-pushes can add push directions to a block or turn a wall into a one-way-pushable block). Undo moves can delete walls in two ways: 1. The player steps backward into a wall and it disappears because it's now involved in the solution 2. The player "pulls" a wall to make it a block instead.

After making an undo move, we have to check that the intended solution (implied by the sequence of reverse moves) is an optimal forward solve. To illustrate, let's look at an example.

<img width="502" height="515" alt="image" src="https://github.com/user-attachments/assets/08975357-7e54-4fb6-ac9f-f4421dc3cdab" />

This 2-step level has 7 possible undo moves: undo-walk and undo-push for each of 4 directions except for undo-right-push, since there is no block or wall to the right of player to "pull".

However, 4 of those 7 moves don't correspond to optimal forward solves. For example, here's the undo-left-push child:

<img width="505" height="510" alt="image" src="https://github.com/user-attachments/assets/ea893d61-b0a1-473c-82ed-4f7a0f8257bc" />

In theory, the sequence of moves that got us here is LRD. But the optimal solution is one step only (straight down) so this doesn't get to be part of the backsearch. We prune.

# Our Variation

Often, there are many variations on a level that are fundamentally the same. For example:

<img width="552" height="547" alt="image" src="https://github.com/user-attachments/assets/9d4c8b98-97b6-4e1b-943b-f67a54c203a3" />

The solution here is obvious: go up a few times, left a few times, then up to the exit. But really the blocks we push could have been in several places in their row/column (potentially as a wall if they are at the very end of the push sequence). It wouldn't have affected the movestring of the solution. To be more precise, the left block has 4 possible locations and the up block has 5 possible locations.

What if we try to consider these 20 levels "at once" in some sense? Let's define a new abstraction: a QuantumBlock or a "QB". A QB is supposed to encapsulate all of the possible positions a block could have started at (and where it is currently after some forward steps, but we'll get to that). In our example, we would have two QuantumBlocks:

<img width="552" height="550" alt="image" src="https://github.com/user-attachments/assets/b86c0ba9-1506-45e7-9833-c177824c408b" />

The backward moves can "pull" QBs, potentially adding push directions to them.

# Quantum Forward Solve

In theory this is nice, but how do we check for shortcuts/run forward solves? We'll need to think of a QB not as purely a set of possible positions that gets maintained, but as a **mapping** from the possible QB start positions to the current positions implied by the move sequence so far (as well as the push mask corresponding to the start position).

<img width="552" height="550" alt="image" src="https://github.com/user-attachments/assets/3dd1eb87-4cf8-4171-901e-33c5ef07cd3f" />

In this image, if the player goes up twice, then the mapping is {a->a, b->b, c->c, d->c, e->c}. Note that the mapping can go outside of the starting positions, so the output of the mapping can actually be anywhere in the whole grid and not just the starting set of positions.

Now the key idea: **forward steps can invalidate certain QB configurations** and we can track this during the solve. These invalid configurations will arise either as singletons or as pairs. An invalid singleton occurs when either 1. A player interacts with a QB in a direction that is inconsistent with its push mask 2. A player pushes a QB into a wall or non-Quantum block. An invalid pair occurs when two different QBs have overlap: they can't be in the same place, so the starting positions corresponding to the overlap can't simultaneously be true.

So, as we make forward moves, we have to track these invalidations. When we find a shortcut, we know that any non-shortcut QB assignment has to be invalid for this shortcut! So this means that if we finish our forward solve and find 10 shortcuts, we should prune this board unless we find a QB assignment that invalidates all 10 shortcuts.

There are a lot of details to get right here to make this somewhat efficient. For example, sometimes you can prune nodes in the forward solve because there is no valid QB assignment left. To that end, I store valid assignments in a "witness pool" and whenever I encounter a node in the forward search that breaks its forward witness, I try the witnesses for the other nodes we've encountered in the forward search so far.

After the forward search is done, we should simplify the QBs as much as possible: if all non-shortcut assignments have one QB in a particular slot, we should turn it into a concrete block before going further into the backsolve tree. There may be other opportunities to optimize here; it's a tricky balance/decision for how much to keep everything quantum vs. splitting up into multiple boards that cover the non-shortcut assignments.

The devil is in the details here (there's a good amount I didn't dive into) and I'm sure the LLMs took it farther than I had considered, but hopefully the overall idea makes sense.

# Usage

See BUILD_AND_USAGE.md. That part was LLM-generated.
