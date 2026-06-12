use std::{collections::hash_map::Entry, fmt};

use ahash::HashMap;

use crate::{
    OperationCount,
    printer::{AtomPrinter, PrintOptions},
};

use super::{Atom, AtomCore, AtomView};

/// An atom that contains opaque aliases, together with a map from the aliases to their original atoms.
/// An aliased atom may have a lower memory footprint than the original atom if it contains many repeated subexpressions.
///
/// Use [AtomCore::alias_subexpressions] to create an aliased atom, or register aliases manually.
///
///
/// # Examples
///
/// ```
/// use symbolica::prelude::*;
///
/// let a: AliasedAtom = parse!("(x+1)^2+(x+1)^3").into();
/// let b = a.add_alias(parse!("s(1)"), parse!("x+1"));
/// assert!(!b.contains_symbol(symbol!("x")));
/// assert_eq!(b.get_root(), &parse!("s(1)^2+s(1)^3"));
/// ```
#[derive(Clone)]
pub struct AliasedAtom {
    pub(crate) root: Atom,
    pub(crate) aliases: HashMap<Atom, Atom>, // TODO: Rc?
}

impl std::fmt::Display for AliasedAtom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        AtomPrinter::new(self.get_root().as_view()).fmt(f)
    }
}

impl fmt::Debug for AliasedAtom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let aliases = self.sorted_aliases();

        if f.alternate() {
            writeln!(f, "{{")?;
            write!(f, "    {}", AtomPrinter::new(self.root.as_view()))?;
            for (alias, original) in aliases {
                writeln!(f, ",")?;
                write!(
                    f,
                    "    {}={}",
                    AtomPrinter::new(alias.as_view()),
                    AtomPrinter::new(original.as_view())
                )?;
            }
            write!(f, "\n}}")
        } else {
            write!(f, "{{{}", AtomPrinter::new(self.root.as_view()))?;
            for (alias, original) in aliases {
                write!(
                    f,
                    ", {}={}",
                    AtomPrinter::new(alias.as_view()),
                    AtomPrinter::new(original.as_view())
                )?;
            }
            write!(f, "}}")
        }
    }
}

impl Default for AliasedAtom {
    /// Create an aliased atom that represents the number 0.
    #[inline]
    fn default() -> Self {
        AliasedAtom {
            root: Atom::Zero,
            aliases: HashMap::default(),
        }
    }
}

impl AliasedAtom {
    /// Get the root atom, which may contain aliases.
    pub fn get_root(&self) -> &Atom {
        &self.root
    }

    /// Get the map from aliases to their original atoms. These atoms may contain aliases themselves.
    pub fn get_aliases(&self) -> &HashMap<Atom, Atom> {
        &self.aliases
    }

    /// Return the number of bytes stored in the root atom and alias definitions.
    pub fn get_byte_size(&self) -> usize {
        self.root.as_view().get_byte_size()
            + self
                .aliases
                .iter()
                .map(|(alias, original)| {
                    alias.as_view().get_byte_size() + original.as_view().get_byte_size()
                })
                .sum::<usize>()
    }

    /// Print the root atom and alias definitions with the given options.
    pub fn printer(&self, opts: PrintOptions) -> AliasedAtomPrinter<'_> {
        AliasedAtomPrinter {
            atom: self,
            print_opts: opts,
        }
    }

    /// Map the root atom using the given function.
    pub fn map_root(self, f: impl FnOnce(Atom) -> Atom) -> Self {
        Self {
            root: f(self.root),
            aliases: self.aliases,
        }
    }

    /// Return the root atom and the alias map.
    pub fn into_inner_with_aliases(self) -> (Atom, HashMap<Atom, Atom>) {
        (self.root, self.aliases)
    }

    /// Undo the common subexpression extraction and return the original atom.
    pub fn into_inner(mut self) -> Atom {
        // TODO: this can be a one-pass if unfolded in reverse insertion order
        loop {
            let out = self.root.replace_map(|a, _, out| {
                if let Some(replacement) = self.aliases.get::<[u8]>(a.get_data()) {
                    out.set_from_view(&replacement.as_view());
                }
            });

            if out != self.root {
                self.root = out;
            } else {
                break;
            }
        }

        self.root
    }

    /// Extract common subexpressions from the root atom and replace them with aliases, which are returned in the map.
    pub fn alias_subexpressions(
        self,
        f: impl FnMut(AtomView, usize, usize) -> Option<Atom>,
    ) -> Self {
        // FIXME: do in one pass
        self.into_inner().as_atom_view().alias_subexpressions(f)
    }

    /// Register an alias, but do not apply it to the root atom.
    pub fn register_alias(&mut self, alias: Atom, original: Atom) {
        match self.aliases.entry(alias) {
            Entry::Occupied(entry) => {
                if entry.get() != &original {
                    panic!(
                        "Redefined alias: {} -> {} vs {}",
                        entry.key(),
                        entry.get(),
                        original
                    );
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(original);
            }
        }
    }

    /// Register an alias for an atom and apply the substitution to the root.
    pub fn add_alias(mut self, alias: Atom, original: Atom) -> Self {
        let new_root = self.root.replace_map(|a, _, out| {
            if a == original.as_view() {
                out.set_from_view(&alias.as_view());
            }
        });

        self.register_alias(alias, original);

        Self {
            root: new_root,
            aliases: self.aliases,
        }
    }

    /// Apply currently registered aliases to the root atom.
    pub fn apply_aliases(&self) -> Self {
        let inv_map = self
            .aliases
            .iter()
            .map(|(a, b)| (b.as_view(), a.as_view()))
            .collect::<HashMap<_, _>>();

        let new_root = self.root.replace_map(|a, _, out| {
            if let Some(alias) = inv_map.get(&a) {
                out.set_from_view(alias);
            }
        });

        Self {
            root: new_root,
            aliases: self.aliases.clone(),
        }
    }

    /// Return the number of operations needed to evaluate the aliased atom.
    pub fn count_operations(&self) -> OperationCount {
        let mut count = OperationCount::default();

        let mut counter = |a: AtomView<'_>| match a {
            AtomView::Mul(m) => {
                count.multiplications += m.get_nargs() - 1;
                true
            }
            AtomView::Add(a) => {
                count.additions += a.get_nargs() - 1;
                true
            }
            AtomView::Pow(p) => {
                if let Ok(i) = isize::try_from(p.get_exp()) {
                    count.add_integer_power(i as i64);
                } else {
                    count.add_function_call();
                }
                true
            }
            _ => true,
        };

        self.root.visitor(&mut counter);

        for x in self.aliases.values() {
            x.visitor(&mut counter);
        }

        count
    }

    /// Removes any aliases in the root whose body has the same symmetric operator as its use in the root.
    /// For example `x+y+s(1), s(1)=z+w` will yield `x+y+z+w`. This is important to maintain invariants in pattern matching, such
    /// as argument counts.
    pub fn flatten_operators(&mut self) {
        self.root = self.root.replace_map(|a, ctx, out| {
            if let Some(alias) = self.aliases.get::<[u8]>(a.get_data()) {
                if ctx.parent_type == Some(alias.get_atom_type()) {
                    out.set_from_view(&alias.as_atom_view());
                }
            }
        });
    }

    /// Removes any aliases that are not used in the root or any of the aliases.
    pub fn prune(&mut self) {
        let mut to_remove = Vec::new();

        for alias in self.aliases.keys() {
            if !self.root.contains(alias) && self.aliases.values().all(|a| !a.contains(alias)) {
                to_remove.push(alias.clone());
            }
        }

        let prunable = !to_remove.is_empty();
        for alias in to_remove {
            self.aliases.remove(&alias);
        }

        if prunable {
            // recursively prune until no more aliases are removable
            self.prune();
        }
    }

    fn sorted_aliases(&self) -> Vec<(&Atom, &Atom)> {
        let mut aliases: Vec<_> = self.aliases.iter().collect();
        aliases.sort_by_cached_key(|(alias, _)| format!("{}", AtomPrinter::new(alias.as_view())));
        aliases
    }
}

/// A printer for aliased atoms, useful in a [format!].
///
/// # Examples
///
/// ```
/// use symbolica::prelude::*;
/// let a = parse!("(x+1)^2+(x+1)^3")
///     .alias_subexpressions(|_a, count, i| (count > 1).then(|| function!(symbol!("se"), i)));
/// println!("{}", a.printer(PrintOptions::file()));
/// ```
pub struct AliasedAtomPrinter<'a> {
    atom: &'a AliasedAtom,
    print_opts: PrintOptions,
}

impl fmt::Display for AliasedAtomPrinter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let aliases = self.atom.sorted_aliases();

        if f.alternate() {
            writeln!(f, "{{")?;
            write!(
                f,
                "    {}",
                AtomPrinter::new_with_options(self.atom.root.as_view(), self.print_opts.clone())
            )?;
            for (alias, original) in aliases {
                writeln!(f, ",")?;
                write!(
                    f,
                    "    {}={}",
                    AtomPrinter::new_with_options(alias.as_view(), self.print_opts.clone()),
                    AtomPrinter::new_with_options(original.as_view(), self.print_opts.clone())
                )?;
            }
            write!(f, "\n}}")
        } else {
            write!(
                f,
                "{{{}",
                AtomPrinter::new_with_options(self.atom.root.as_view(), self.print_opts.clone())
            )?;
            for (alias, original) in aliases {
                write!(
                    f,
                    ", {}={}",
                    AtomPrinter::new_with_options(alias.as_view(), self.print_opts.clone()),
                    AtomPrinter::new_with_options(original.as_view(), self.print_opts.clone())
                )?;
            }
            write!(f, "}}")
        }
    }
}

impl From<Atom> for AliasedAtom {
    fn from(atom: Atom) -> Self {
        AliasedAtom {
            root: atom,
            aliases: HashMap::default(),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{atom::AtomCore, function, id::AliasedAtom, parse, symbol};

    #[test]
    fn create_alias() {
        let a: AliasedAtom = parse!("(x+1)^2+(x+1)^3").into();
        let b = a.add_alias(parse!("s(1)"), parse!("x+1"));
        let c = b + 2 + parse!("(x+1)^4");
        assert!(c.contains_symbol(symbol!("x")));
        let d = c.apply_aliases();
        assert!(!d.contains_symbol(symbol!("x")));
    }

    #[test]
    fn flatten() {
        let mut aliased: AliasedAtom = parse!("x+y+s(1)").into();
        aliased.register_alias(parse!("s(1)"), parse!("z+w"));
        aliased.flatten_operators();
        assert_eq!(aliased.get_root(), &parse!("x+y+z+w"));
    }

    fn aliased_atom() -> super::AliasedAtom {
        parse!("(x+1)^2+(x+1)^3")
            .alias_subexpressions(|_a, count, i| (count > 1).then(|| function!(symbol!("se"), i)))
    }

    #[test]
    fn byte_size_counts_root_and_aliases() {
        let aliased = aliased_atom();
        let expected = aliased.get_root().as_view().get_byte_size()
            + aliased
                .get_aliases()
                .iter()
                .map(|(alias, original)| {
                    alias.as_view().get_byte_size() + original.as_view().get_byte_size()
                })
                .sum::<usize>();

        assert_eq!(aliased.get_byte_size(), expected);
    }

    #[test]
    fn binary_ops_with_atoms_and_symbols_preserve_aliases() {
        let aliased = aliased_atom();
        let expected = parse!("(x+1)^2+(x+1)^3+y");

        let with_symbol_rhs = &aliased + symbol!("y");
        assert_eq!(with_symbol_rhs.get_aliases(), aliased.get_aliases());
        assert_eq!(with_symbol_rhs.into_inner(), expected);

        let with_atom_lhs = parse!("y") + &aliased;
        assert_eq!(with_atom_lhs.get_aliases(), aliased.get_aliases());
        assert_eq!(with_atom_lhs.into_inner(), expected);

        let with_symbol_lhs = symbol!("y") + &aliased;
        assert_eq!(with_symbol_lhs.get_aliases(), aliased.get_aliases());
        assert_eq!(with_symbol_lhs.into_inner(), expected);
    }
}
