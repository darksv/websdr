pub(crate) struct ArrayWindows<I: Iterator, const N: usize> {
    iter: I,
    current: [I::Item; N],
    empty: bool,
}

impl<I: Iterator, const N: usize> Iterator for ArrayWindows<I, N>
where
    I::Item: Copy,
{
    type Item = [I::Item; N];

    fn next(&mut self) -> Option<[I::Item; N]> {
        if self.empty {
            return None;
        }

        let c = self.current;
        self.current.rotate_left(1);
        self.current[N - 1] = self.iter.next()?;
        Some(c)
    }
}

pub(crate) trait IterExt: Iterator + Sized {
    fn array_windows<const N: usize>(mut self) -> ArrayWindows<Self, N>
    where
        Self::Item: Copy + Default,
    {
        let mut is_empty = false;
        ArrayWindows {
            current: std::array::from_fn(|_| {
                let val = self.next();
                if val.is_none() {
                    is_empty = true;
                }
                val.unwrap_or(Self::Item::default())
            }),
            iter: self,
            empty: is_empty,
        }
    }
}

impl<I> IterExt for I where I: Iterator {}
