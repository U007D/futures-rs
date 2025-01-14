//! Definition of the `JoinAll` combinator, waiting for all of a list of futures
//! to finish.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::iter::FromIterator;
use core::mem;
use core::pin::Pin;
use core::task::{Context, Poll};

use super::{assert_future, MaybeDone};

#[cfg(not(futures_no_atomic_cas))]
use crate::stream::{Collect, FuturesOrdered, StreamExt};

pub(crate) fn iter_pin_mut<T>(slice: Pin<&mut [T]>) -> impl Iterator<Item = Pin<&mut T>> {
    // Safety: `std` _could_ make this unsound if it were to decide Pin's
    // invariants aren't required to transmit through slices. Otherwise this has
    // the same safety as a normal field pin projection.
    unsafe { slice.get_unchecked_mut() }.iter_mut().map(|t| unsafe { Pin::new_unchecked(t) })
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
/// Future for the [`join_all`] function.
pub struct JoinAll<F>
where
    F: Future,
{
    kind: JoinAllKind<F>,
}

#[cfg(not(futures_no_atomic_cas))]
pub(crate) const SMALL: usize = 30;

enum JoinAllKind<F>
where
    F: Future,
{
    Small {
        elems: Pin<Box<[MaybeDone<F>]>>,
    },
    #[cfg(not(futures_no_atomic_cas))]
    Big {
        fut: Collect<FuturesOrdered<F>, Vec<F::Output>>,
    },
}

impl<F> fmt::Debug for JoinAll<F>
where
    F: Future + fmt::Debug,
    F::Output: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            JoinAllKind::Small { ref elems } => {
                f.debug_struct("JoinAll").field("elems", elems).finish()
            }
            #[cfg(not(futures_no_atomic_cas))]
            JoinAllKind::Big { ref fut, .. } => fmt::Debug::fmt(fut, f),
        }
    }
}

/// Creates a future which represents a collection of the outputs of the futures
/// given.
///
/// The returned future will drive execution for all of its underlying futures,
/// collecting the results into a destination `Vec<T>` in the same order as they
/// were provided.
///
/// This function is only available when the `std` or `alloc` feature of this
/// library is activated, and it is activated by default.
///
/// # See Also
///
/// `join_all` will switch to the more powerful [`FuturesOrdered`] for performance
/// reasons if the number of futures is large. You may want to look into using it or
/// it's counterpart [`FuturesUnordered`][crate::stream::FuturesUnordered] directly.
///
/// Some examples for additional functionality provided by these are:
///
///  * Adding new futures to the set even after it has been started.
///
///  * Only polling the specific futures that have been woken. In cases where
///    you have a lot of futures this will result in much more efficient polling.
///
/// When awaiting a collection of spawned tasks, consider iterating over them with
/// a for loop instead of using `join_all`, since `join_all` is more expensive than
/// simple iteration and will not warn you if there are errors.
///
/// This extra expense come from `join_all` being designed to work with all types of
/// futures.  For most futures, simply iterating over a collection of them would make
/// the futures run sequentially. To ensure they always run at the same time,
/// `join_all` must do something significantly more complicated.
///
/// Consider using `join_all` if the following conditions are met:
///     1. You don't want to spawn the tasks. (Usually because they are not `'static`),
///     2. You care about getting their return values back in the same order, and you
///        can't use any of them until you have all of them, and
///     3. You don't want any limit on the number of futures running at the same time.
///
/// # Examples
///
/// ```
/// # futures::executor::block_on(async {
/// use futures::future::join_all;
///
/// async fn foo(i: u32) -> u32 { i }
///
/// let futures = vec![foo(1), foo(2), foo(3)];
///
/// assert_eq!(join_all(futures).await, [1, 2, 3]);
/// # });
/// ```
pub fn join_all<I>(iter: I) -> JoinAll<I::Item>
where
    I: IntoIterator,
    I::Item: Future,
{
    let iter = iter.into_iter();

    #[cfg(futures_no_atomic_cas)]
    {
        let kind =
            JoinAllKind::Small { elems: iter.map(MaybeDone::Future).collect::<Box<[_]>>().into() };

        assert_future::<Vec<<I::Item as Future>::Output>, _>(JoinAll { kind })
    }

    #[cfg(not(futures_no_atomic_cas))]
    {
        let kind = match iter.size_hint().1 {
            Some(max) if max <= SMALL => JoinAllKind::Small {
                elems: iter.map(MaybeDone::Future).collect::<Box<[_]>>().into(),
            },
            _ => JoinAllKind::Big { fut: iter.collect::<FuturesOrdered<_>>().collect() },
        };

        assert_future::<Vec<<I::Item as Future>::Output>, _>(JoinAll { kind })
    }
}

impl<F> Future for JoinAll<F>
where
    F: Future,
{
    type Output = Vec<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.kind {
            JoinAllKind::Small { elems } => {
                let mut all_done = true;

                for elem in iter_pin_mut(elems.as_mut()) {
                    if elem.poll(cx).is_pending() {
                        all_done = false;
                    }
                }

                if all_done {
                    let mut elems = mem::replace(elems, Box::pin([]));
                    let result =
                        iter_pin_mut(elems.as_mut()).map(|e| e.take_output().unwrap()).collect();
                    Poll::Ready(result)
                } else {
                    Poll::Pending
                }
            }
            #[cfg(not(futures_no_atomic_cas))]
            JoinAllKind::Big { fut } => Pin::new(fut).poll(cx),
        }
    }
}

impl<F: Future> FromIterator<F> for JoinAll<F> {
    fn from_iter<T: IntoIterator<Item = F>>(iter: T) -> Self {
        join_all(iter)
    }
}
